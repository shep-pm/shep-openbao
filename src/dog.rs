//! The loop around [`Mirror`]: rounds on a timer, config changes as they
//! arrive, and a forced round after every reconnect.
//!
//! A reconnect forces a push because the shepherd on the other end may be a
//! new one. With `persist = false` it holds nothing this dog pushed before,
//! and every sheep reading `{{secret:<name>/...}}` would wait on the next
//! change in OpenBao, which may never come.

use std::{
    process::ExitCode,
    time::{Duration, Instant},
};

use shep_client::{
    EventStream, LinkLost, RECONNECT_MIN_DELAY, ReconnectingClient, RequestError,
    dogs::{Interrupted, ShepherdError, Stop, parse_section},
    shep_core::{exit, protocol::BusEvent, values::UpDuration},
};

use crate::{
    config::{Config, Section},
    run::{Mirror, Outcome, chain},
    shepherd::Link,
};

/// How long the dog waits for its shepherd to come back after the
/// connection drops. shep's own dogs use the daemon's `DOG_SILENCE_BUDGET`,
/// five seconds, which shep-daemon does not export to a dog outside the
/// workspace; this matches it so this dog is judged by the same clock.
const SHEPHERD_RETURN_BUDGET: Duration = Duration::from_secs(5);

/// Runs until a stop signal, or until the shepherd is gone for good.
pub async fn run(
    client: &ReconnectingClient,
    name: String,
    config: Config,
    mut stop: Stop,
) -> ExitCode {
    let mut mirror = match Mirror::new(Link::new(client, name.clone()), config) {
        Ok(mirror) => mirror,
        Err(err) => {
            eprintln!("shep-openbao: {}", chain(&err));
            return exit::INVALID_CONFIG.into();
        }
    };
    println!("{}", describe(mirror.config(), &name));
    let topics = vec![format!("config.dog.{name}")];
    let mut events = match client.subscribe(topics.clone()).await {
        Ok(events) => events,
        Err(err) => {
            eprintln!("shep-openbao: cannot hear config changes: {err}");
            return err.exit_code().into();
        }
    };
    let mut force = false;
    let mut next = Instant::now();
    loop {
        let wait = next.saturating_duration_since(Instant::now());
        tokio::select! {
            interrupted = stop.sleep(wait) => {
                if interrupted == Interrupted::Yes {
                    return ExitCode::SUCCESS;
                }
                // A round can wait on OpenBao for a whole request timeout
                // per path, so a stop is heard during it, not after.
                // Dropping it midway is safe: nothing is recorded as pushed
                // until the shepherd has answered.
                let outcomes = tokio::select! {
                    biased;
                    () = stop.wait() => return ExitCode::SUCCESS,
                    outcomes = mirror.round(force) => outcomes,
                };
                log(&outcomes);
                force = false;
                next = Instant::now() + mirror.config().interval;
            }
            event = events.next() => match event {
                Some(Ok(BusEvent::DogConfigChanged { dog })) if dog == name => {
                    reload(&mut mirror, client, &name).await;
                    next = Instant::now();
                }
                Some(Ok(_)) => {}
                // Events were dropped, and one of them may have been a
                // config change, so read the section again to be sure.
                Some(Err(_lagged)) => {
                    reload(&mut mirror, client, &name).await;
                    next = Instant::now();
                }
                None => {
                    // A stop during the wait is a stop, not a lost
                    // shepherd: a shepherd shutting down closes the socket
                    // and signals its dogs at about the same moment, and
                    // exiting on the budget would report a failure.
                    let resubscribed = tokio::select! {
                        biased;
                        () = stop.wait() => return ExitCode::SUCCESS,
                        resubscribed = resubscribe(client, &topics) => resubscribed,
                    };
                    events = match resubscribed {
                        Ok(events) => events,
                        Err(err) => {
                            eprintln!("shep-openbao: {err}");
                            return err.exit_code().into();
                        }
                    };
                    println!("reconnected to the shepherd, pushing every environment again");
                    // A new shepherd may also be carrying a changed dogs.toml.
                    reload(&mut mirror, client, &name).await;
                    force = true;
                    next = Instant::now();
                }
            },
        }
    }
}

/// The line the dog starts with: what it will do, and where the pushes go.
fn describe(config: &Config, name: &str) -> String {
    let every =
        UpDuration::from_millis(u64::try_from(config.interval.as_millis()).unwrap_or(u64::MAX));
    let cache = if config.persist {
        "the shepherd caches pushes in secrets-cache.json"
    } else {
        "the shepherd keeps pushes in memory only (persist = false)"
    };
    match config.environments.len() {
        0 => format!("no environments in [{name}], so nothing to push yet; {cache}"),
        n => format!(
            "mirroring {n} environment{} into namespace `{name}` every {every}; {cache}",
            if n == 1 { "" } else { "s" }
        ),
    }
}

fn log(outcomes: &[Outcome]) {
    for outcome in outcomes.iter().filter(|o| o.worth_logging()) {
        println!("{outcome}");
    }
}

/// Reads the section again and applies it. A section that no longer parses
/// or resolves leaves the running settings in force, so a typo in
/// dogs.toml cannot stop the pushes.
async fn reload(mirror: &mut Mirror<Link<'_>>, client: &ReconnectingClient, name: &str) {
    let section = match client.dog_config(name).await {
        Ok(section) => section,
        Err(err) => {
            eprintln!("config change not read: {err}; keeping the previous settings");
            return;
        }
    };
    let config = match parse_section::<Section>(name, section.as_str()) {
        Ok(section) => match section.resolve() {
            Ok(config) => config,
            Err(err) => {
                eprintln!("config change not applied: {err}; keeping the previous settings");
                return;
            }
        },
        Err(err) => {
            eprintln!("config change not applied: {err}; keeping the previous settings");
            return;
        }
    };
    if config == *mirror.config() {
        return;
    }
    match mirror.reconfigure(config).await {
        Ok(outcomes) => {
            println!(
                "applied a config change: {}",
                describe(mirror.config(), name)
            );
            log(&outcomes);
        }
        Err(err) => eprintln!(
            "config change not applied: {}; keeping the previous settings",
            chain(&err)
        ),
    }
}

/// Waits for the shepherd to come back and subscribes again, within
/// [`SHEPHERD_RETURN_BUDGET`]. The same shape as shep's own bark dog.
async fn resubscribe(
    client: &ReconnectingClient,
    topics: &[String],
) -> Result<EventStream, ShepherdError> {
    let started = Instant::now();
    let left = || SHEPHERD_RETURN_BUDGET.saturating_sub(started.elapsed());
    let lost = || {
        ShepherdError::Lost(LinkLost::Budget {
            waited: started.elapsed(),
        })
    };
    loop {
        if left().is_zero() {
            return Err(lost());
        }
        client
            .connected_within(left())
            .await
            .map_err(ShepherdError::Lost)?;
        if left().is_zero() {
            return Err(lost());
        }
        match tokio::time::timeout(left(), client.subscribe(topics.to_vec())).await {
            Ok(Ok(events)) => return Ok(events),
            // Issued on a generation that had already died: the supervisor
            // is about to say so, and the budget decides whether to go on.
            Ok(Err(RequestError::Closed)) => {}
            Ok(Err(other)) => return Err(ShepherdError::Request(other)),
            Err(_elapsed) => return Err(lost()),
        }
        // The supervisor notices a dead connection a moment after the
        // socket does, so a bare retry would spin against a link that
        // still reads as connected.
        tokio::time::sleep(RECONNECT_MIN_DELAY.min(left())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Section;

    fn config(extra: &str) -> Config {
        let text = format!(
            "address = \"https://openbao.example.com\"\nrole_id = \"r\"\nsecret_id = \"s\"\n{extra}"
        );
        parse_section::<Section>("openbao", &text)
            .expect("parses")
            .resolve()
            .expect("resolves")
    }

    #[test]
    fn the_first_line_says_what_the_dog_will_do() {
        assert_eq!(
            describe(
                &config("[environments.production]\npaths = [\"a\"]"),
                "openbao"
            ),
            "mirroring 1 environment into namespace `openbao` every 5m; the shepherd caches pushes in secrets-cache.json"
        );
        assert_eq!(
            describe(
                &config(
                    "persist = false\ninterval = \"90s\"\n[environments.a]\npaths = [\"a\"]\n[environments.b]\npaths = [\"b\"]"
                ),
                "vault"
            ),
            "mirroring 2 environments into namespace `vault` every 90s; the shepherd keeps pushes in memory only (persist = false)"
        );
        assert_eq!(
            describe(&config(""), "openbao"),
            "no environments in [openbao], so nothing to push yet; the shepherd caches pushes in secrets-cache.json"
        );
    }
}
