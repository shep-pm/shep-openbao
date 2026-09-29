//! [`Mirror`], this dog's state between rounds, and what one round does.
//!
//! A round logs in when the token is missing or due, reads each environment's
//! KV paths, builds its set, and pushes it when it differs from the last push
//! that landed, or when `force` says the shepherd may have lost it. Nothing a
//! round finds wrong changes what the shepherd holds: a failed read, a
//! refused set or a refused push leaves the last push in place, and the next
//! round tries again.

use core::fmt;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use crate::{
    bao::{Bao, BaoError},
    config::{Config, Environment},
    mirror::{self, Set},
    shepherd::Shepherd,
};

/// What happened to one environment in a round, or in a reconfigure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A set landed. Carries its key names, never a value.
    Pushed {
        environment: String,
        keys: Vec<String>,
    },
    /// The set matched the last push, so nothing was sent.
    Unchanged { environment: String },
    /// No set was built, so nothing was sent. Each reason names a path or a
    /// key.
    Skipped {
        environment: String,
        reasons: Vec<String>,
    },
    /// A set was built and the shepherd did not take it.
    Refused { environment: String, error: String },
    /// The environment left `[openbao]`, so an empty set replaced it.
    Emptied { environment: String },
}

impl Outcome {
    /// Whether this is worth a log line. An unchanged environment is not: at
    /// the default interval it would be one line per environment every five
    /// minutes saying nothing.
    #[must_use]
    pub fn worth_logging(&self) -> bool {
        !matches!(self, Self::Unchanged { .. })
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pushed { environment, keys } if keys.is_empty() => {
                write!(f, "{environment}: pushed an empty set")
            }
            Self::Pushed { environment, keys } => write!(
                f,
                "{environment}: pushed {} secret{}: {}",
                keys.len(),
                if keys.len() == 1 { "" } else { "s" },
                keys.join(", ")
            ),
            Self::Unchanged { environment } => write!(f, "{environment}: unchanged"),
            Self::Skipped {
                environment,
                reasons,
            } => write!(
                f,
                "{environment}: kept the last push: {}",
                reasons.join("; ")
            ),
            Self::Refused { environment, error } => {
                write!(
                    f,
                    "{environment}: the shepherd did not take the push: {error}"
                )
            }
            Self::Emptied { environment } => write!(
                f,
                "{environment}: pushed an empty set, since it left [openbao]"
            ),
        }
    }
}

/// This dog between rounds.
#[derive(Debug)]
pub struct Mirror<S> {
    shepherd: S,
    bao: Bao,
    config: Config,
    token: Option<crate::bao::Token>,
    /// The last set that landed for each environment. What a round compares
    /// against, and what tells a reconfigure which environments to empty.
    pushed: BTreeMap<String, Set>,
    /// Environments the shepherd may not hold the last set for: one whose
    /// push failed, or every one after a reconnect. Each is pushed at its
    /// next successful read whatever `pushed` says, and leaves this set only
    /// when a push lands. Without it, a forced round that failed would be
    /// followed by rounds that find the set unchanged and never send it.
    dirty: BTreeSet<String>,
}

impl<S: Shepherd> Mirror<S> {
    /// A mirror with nothing pushed yet.
    ///
    /// # Errors
    ///
    /// [`BaoError`] when the OpenBao client cannot be built from `config`,
    /// such as an unreadable `ca_cert`.
    pub fn new(shepherd: S, config: Config) -> Result<Self, BaoError> {
        let bao = bao_for(&config)?;
        Ok(Self {
            shepherd,
            bao,
            config,
            token: None,
            pushed: BTreeMap::new(),
            dirty: BTreeSet::new(),
        })
    }

    /// The settings in force.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Reads every environment and pushes what changed. `force` marks every
    /// environment dirty first, so each is pushed at its next successful
    /// read, in this round or a later one.
    pub async fn round(&mut self, force: bool) -> Vec<Outcome> {
        let environments: Vec<(String, Environment)> = self
            .config
            .environments
            .iter()
            .map(|(name, environment)| (name.clone(), environment.clone()))
            .collect();
        if force {
            self.dirty
                .extend(environments.iter().map(|(name, _)| name.clone()));
        }
        // Before anything that needs OpenBao: emptying an environment that
        // left the config needs only the shepherd.
        let mut outcomes = self.empty_removed().await;
        if environments.is_empty() {
            return outcomes;
        }
        // One login per round rather than one per environment, so a
        // server that refuses it is asked once, not once per environment.
        if let Err(err) = self.ensure_token().await {
            let reason = chain(&err);
            outcomes.extend(
                environments
                    .into_iter()
                    .map(|(environment, _)| Outcome::Skipped {
                        environment,
                        reasons: vec![reason.clone()],
                    }),
            );
            return outcomes;
        }
        for (name, environment) in environments {
            outcomes.push(self.environment(name, &environment).await);
        }
        outcomes
    }

    /// Swaps in `config`, emptying every environment that left it.
    ///
    /// A changed address, CA or OpenBao namespace rebuilds the client, and
    /// changed login fields drop the token so the next round logs in with
    /// them. Environments that stayed are left for the next round, which
    /// pushes whatever their new paths hold.
    ///
    /// # Errors
    ///
    /// [`BaoError`] when the client cannot be rebuilt, in which case the
    /// previous settings stay in force entirely.
    pub async fn reconfigure(&mut self, config: Config) -> Result<Vec<Outcome>, BaoError> {
        let reach_changed = config.address != self.config.address
            || config.ca_cert != self.config.ca_cert
            || config.openbao_namespace != self.config.openbao_namespace;
        if reach_changed {
            self.bao = bao_for(&config)?;
            self.token = None;
        }
        if config.role_id != self.config.role_id || config.secret_id != self.config.secret_id {
            self.token = None;
        }
        self.config = config;
        Ok(self.empty_removed().await)
    }

    /// Pushes an empty set for every environment this run pushed that has
    /// since left the config. One whose empty push fails stays in `pushed`,
    /// so every round tries it again until it lands: the shepherd would
    /// otherwise keep serving its old values indefinitely.
    async fn empty_removed(&mut self) -> Vec<Outcome> {
        let gone: Vec<String> = self
            .pushed
            .keys()
            .filter(|name| !self.config.environments.contains_key(*name))
            .cloned()
            .collect();
        let mut outcomes = Vec::with_capacity(gone.len());
        for environment in gone {
            match self.shepherd.push(&environment, &Set::new()).await {
                Ok(_) => {
                    self.pushed.remove(&environment);
                    self.dirty.remove(&environment);
                    outcomes.push(Outcome::Emptied { environment });
                }
                Err(err) => outcomes.push(Outcome::Refused {
                    environment,
                    error: err.to_string(),
                }),
            }
        }
        outcomes
    }

    async fn environment(&mut self, name: String, environment: &Environment) -> Outcome {
        let set = match self.read(environment).await {
            Ok(set) => set,
            Err(reasons) => {
                return Outcome::Skipped {
                    environment: name,
                    reasons,
                };
            }
        };
        if !self.dirty.contains(&name) && self.pushed.get(&name) == Some(&set) {
            return Outcome::Unchanged { environment: name };
        }
        match self.shepherd.push(&name, &set).await {
            Ok(_) => {
                let keys = set.keys().cloned().collect();
                self.dirty.remove(&name);
                self.pushed.insert(name.clone(), set);
                Outcome::Pushed {
                    environment: name,
                    keys,
                }
            }
            Err(err) => {
                self.dirty.insert(name.clone());
                Outcome::Refused {
                    environment: name,
                    error: err.to_string(),
                }
            }
        }
    }

    /// Every path of `environment` read and built into one set. A 403 on any
    /// path logs in again and reads the whole environment once more, since a
    /// token OpenBao revoked early answers 403 the same way a policy that
    /// does not reach the path does, and only a fresh token tells them apart.
    async fn read(&mut self, environment: &Environment) -> Result<Set, Vec<String>> {
        let mut retried = false;
        loop {
            self.ensure_token().await.map_err(|err| vec![chain(&err)])?;
            let Some(token) = self.token.as_ref() else {
                return Err(vec!["no token after logging in".to_string()]);
            };
            let mut reads = Vec::with_capacity(environment.paths.len());
            let mut failures = Vec::new();
            let mut forbidden = false;
            for path in &environment.paths {
                match self.bao.read(token, &environment.mount, path).await {
                    Ok(values) => reads.push((format!("{}/{path}", environment.mount), values)),
                    Err(err) => {
                        forbidden |= matches!(err, BaoError::Forbidden(_));
                        failures.push(chain(&err));
                    }
                }
            }
            if forbidden && !retried {
                retried = true;
                self.token = None;
                continue;
            }
            if !failures.is_empty() {
                return Err(failures);
            }
            return mirror::build(reads)
                .map_err(|problems| problems.iter().map(ToString::to_string).collect());
        }
    }

    async fn ensure_token(&mut self) -> Result<(), BaoError> {
        if self
            .token
            .as_ref()
            .is_some_and(|token| !token.due(Instant::now()))
        {
            return Ok(());
        }
        self.token = None;
        let token = self
            .bao
            .login(&self.config.role_id, &self.config.secret_id)
            .await?;
        self.token = Some(token);
        Ok(())
    }
}

fn bao_for(config: &Config) -> Result<Bao, BaoError> {
    Bao::new(
        config.address.clone(),
        config.ca_cert.as_deref(),
        config.openbao_namespace.clone(),
    )
}

/// `err` and every source under it, joined with `: `. Every error this
/// crate prints has been written not to carry a value, and a source is
/// either one of those or reqwest's, which names a URL and never a body.
#[must_use]
pub fn chain(err: &dyn core::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(next) = source {
        out.push_str(": ");
        out.push_str(&next.to_string());
        source = next.source();
    }
    out
}

#[cfg(test)]
mod tests;
