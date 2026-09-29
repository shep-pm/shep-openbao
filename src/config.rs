//! The `[openbao]` section of `dogs.toml`, and the settings it resolves to.
//!
//! The shepherd serves the section over the socket rather than through the
//! environment, since it holds the AppRole secret ID. [`Section`] is what an
//! operator may write, every field optional so a half-written section still
//! parses and the error names the missing key. [`Section::resolve`] turns it
//! into a [`Config`] with every default decided and every rule checked.

use core::{fmt, time::Duration};
use std::{collections::BTreeMap, net::IpAddr, path::PathBuf};

use reqwest::Url;
use schemars::JsonSchema;
use serde::Deserialize;
use shep_client::{
    dogs::dog_config,
    shep_core::{secrets::is_name, values::UpDuration},
};

use crate::secret::Secret;

/// How often each environment is read when `interval` is unset. The same
/// default OpenBao Agent uses for a static secret.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The KV v2 mount an environment reads from when `mount` is unset: the one
/// `bao server -dev` enables.
pub const DEFAULT_MOUNT: &str = "secret";

/// What `--print-config` prints: the section header, then every setting
/// commented out so the defaults stay the dog's, ready to paste into
/// `$SHEP_HOME/dogs.toml`. `environments` is written inline because each line
/// here has to set exactly one top-level key.
pub const PRINT_CONFIG: &str = "\
[openbao]
#address = \"https://openbao.example.com:8200\"
#ca_cert = \"/etc/ssl/openbao-ca.pem\"
#openbao_namespace = \"team-a\"
#role_id = \"\"
#secret_id = \"\"
#interval = \"5m\"
#persist = true
#environments = { production = { mount = \"secret\", paths = [\"myapp/production\"] } }
";

/// What an operator may write under `[openbao]`.
#[dog_config]
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "openbao",
    description = "Settings for the shep-openbao provider dog."
)]
pub struct Section {
    #[schemars(description = "OpenBao's address, e.g. \"https://openbao.example.com:8200\".")]
    pub address: Option<String>,
    #[schemars(description = "A PEM file with a CA to trust beside the system's own.")]
    pub ca_cert: Option<PathBuf>,
    #[schemars(description = "The OpenBao namespace to work in, for every environment.")]
    pub openbao_namespace: Option<String>,
    #[schemars(description = "The AppRole role ID this dog logs in with.")]
    pub role_id: Option<String>,
    #[shep(secret)]
    #[schemars(
        with = "Option<String>",
        description = "The AppRole secret ID this dog logs in with."
    )]
    pub secret_id: Option<Secret>,
    #[schemars(
        with = "Option<UpDuration>",
        description = "How often each environment is read, e.g. \"5m\"."
    )]
    pub interval: Option<String>,
    // Read by the shepherd, never by this dog: it decides whether pushed
    // values are cached in secrets-cache.json. Declared so `deny_unknown_fields`
    // does not refuse the section over a key the operator was told to write.
    #[schemars(description = "Whether the shepherd caches pushed values on disk. Default true.")]
    pub persist: Option<bool>,
    #[serde(default)]
    #[schemars(description = "The environments to push for, each with the KV paths it mirrors.")]
    pub environments: BTreeMap<String, EnvironmentSection>,
}

/// One `[openbao.environments.<name>]` table.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSection {
    #[schemars(description = "The KV v2 mount to read from. Default \"secret\".")]
    pub mount: Option<String>,
    #[serde(default)]
    #[schemars(description = "The KV paths under the mount whose keys are mirrored.")]
    pub paths: Vec<String>,
}

/// The settings with every default decided.
///
/// `Debug` is derived: the one credential is a [`Secret`], and the address
/// cannot carry one because [`Section::resolve`] refuses a URL with a user
/// or password in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub address: Url,
    pub ca_cert: Option<PathBuf>,
    pub openbao_namespace: Option<String>,
    pub role_id: String,
    pub secret_id: Secret,
    pub interval: Duration,
    /// Whether the shepherd caches what this dog pushes. Only ever logged:
    /// the shepherd reads the same key itself.
    pub persist: bool,
    pub environments: BTreeMap<String, Environment>,
}

/// Where one environment's secrets come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    /// The KV v2 mount, without surrounding slashes.
    pub mount: String,
    /// KV paths under the mount, without surrounding slashes, each listed once.
    pub paths: Vec<String>,
}

/// Why a section does not resolve. Every variant names the key to fix and
/// none carries a value from the section, apart from names and KV paths,
/// which are not secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// A required key is unset.
    Missing(&'static str),
    /// A key is set to an empty string.
    Empty(&'static str),
    /// `address` is not one this dog will talk to, and why.
    Address(&'static str),
    /// `interval` is not a duration shep accepts, or is zero.
    Interval,
    /// An environment name no `{{secret:...}}` reference could ever select.
    EnvironmentName(String),
    /// An environment lists no paths.
    NoPaths(String),
    /// An environment's `mount` is empty, or has an empty, `.` or `..` segment.
    Mount(String),
    /// A path is empty, or has an empty, `.` or `..` segment.
    Path { environment: String, path: String },
    /// A path is listed twice in one environment, so every key at it would
    /// collide with itself.
    DuplicatePath { environment: String, path: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(key) => write!(f, "[openbao] needs `{key}`"),
            Self::Empty(key) => write!(f, "`{key}` in [openbao] is empty"),
            Self::Address(why) => write!(f, "`address` in [openbao] {why}"),
            Self::Interval => f.write_str(
                "`interval` in [openbao] is not a duration shep accepts, or is zero; write one like \"5m\"",
            ),
            Self::EnvironmentName(name) => write!(
                f,
                "environment `{name}` in [openbao] is not a name shep accepts: use letters, digits, `.`, `_` and `-`, not starting with `.`"
            ),
            Self::NoPaths(name) => write!(f, "environment `{name}` in [openbao] lists no paths"),
            Self::Mount(name) => write!(
                f,
                "environment `{name}` in [openbao] has a `mount` with an empty, `.` or `..` segment"
            ),
            Self::Path { environment, path } => write!(
                f,
                "environment `{environment}` in [openbao] has path `{path}`, which is empty or has an empty, `.` or `..` segment"
            ),
            Self::DuplicatePath { environment, path } => write!(
                f,
                "environment `{environment}` in [openbao] lists path `{path}` twice"
            ),
        }
    }
}

impl core::error::Error for ConfigError {}

impl Section {
    /// Decides every default and checks every rule.
    ///
    /// # Errors
    ///
    /// [`ConfigError`], naming the key to fix. The first problem found wins.
    pub fn resolve(self) -> Result<Config, ConfigError> {
        let address = address(self.address)?;
        let role_id = self.role_id.ok_or(ConfigError::Missing("role_id"))?;
        if role_id.is_empty() {
            return Err(ConfigError::Empty("role_id"));
        }
        let secret_id = self.secret_id.ok_or(ConfigError::Missing("secret_id"))?;
        if secret_id.expose().is_empty() {
            return Err(ConfigError::Empty("secret_id"));
        }
        if self.openbao_namespace.as_deref() == Some("") {
            return Err(ConfigError::Empty("openbao_namespace"));
        }
        let interval = match self.interval {
            None => DEFAULT_INTERVAL,
            Some(raw) => raw
                .parse::<UpDuration>()
                .ok()
                .map(UpDuration::as_duration)
                .filter(|d| !d.is_zero())
                .ok_or(ConfigError::Interval)?,
        };
        let mut environments = BTreeMap::new();
        for (name, section) in self.environments {
            let environment = environment(&name, section)?;
            environments.insert(name, environment);
        }
        Ok(Config {
            address,
            ca_cert: self.ca_cert,
            openbao_namespace: self.openbao_namespace,
            role_id,
            secret_id,
            interval,
            persist: self.persist.unwrap_or(true),
            environments,
        })
    }
}

/// `https` anywhere, `http` only to this machine, and nothing that could
/// carry a credential or change where the API lives.
fn address(raw: Option<String>) -> Result<Url, ConfigError> {
    let raw = raw.ok_or(ConfigError::Missing("address"))?;
    // The parse error is dropped rather than quoted: it can echo the input,
    // and an operator who pasted a token into the wrong key would see it.
    let url = Url::parse(&raw).map_err(|_| ConfigError::Address("is not a URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::Address(
            "has a user or password in it; the dog logs in with AppRole",
        ));
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::Address(
            "has a path, query or fragment; give the server's root",
        ));
    }
    let host = url.host_str().ok_or(ConfigError::Address("has no host"))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback(host) => Ok(url),
        "http" => Err(ConfigError::Address(
            "uses http to another machine; use https, or http only to a loopback address",
        )),
        _ => Err(ConfigError::Address("is neither https nor http")),
    }
}

/// `localhost`, or an IPv4 or IPv6 loopback address. `host_str` keeps the
/// brackets around an IPv6 literal.
fn is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn environment(name: &str, section: EnvironmentSection) -> Result<Environment, ConfigError> {
    if !is_name(name) {
        return Err(ConfigError::EnvironmentName(name.to_string()));
    }
    let mount = match section.mount {
        None => DEFAULT_MOUNT.to_string(),
        Some(raw) => clean(&raw).ok_or_else(|| ConfigError::Mount(name.to_string()))?,
    };
    if section.paths.is_empty() {
        return Err(ConfigError::NoPaths(name.to_string()));
    }
    let mut paths: Vec<String> = Vec::with_capacity(section.paths.len());
    for raw in section.paths {
        let path = clean(&raw).ok_or_else(|| ConfigError::Path {
            environment: name.to_string(),
            path: raw.clone(),
        })?;
        if paths.contains(&path) {
            return Err(ConfigError::DuplicatePath {
                environment: name.to_string(),
                path,
            });
        }
        paths.push(path);
    }
    Ok(Environment { mount, paths })
}

/// `raw` without surrounding slashes, or `None` when what is left is empty
/// or has an empty, `.` or `..` segment. Those would read somewhere other
/// than the path the operator wrote.
fn clean(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches('/');
    let fine = !trimmed.is_empty()
        && trimmed
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    fine.then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests;
