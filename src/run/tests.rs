use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use shep_client::RequestError;

use super::*;
use crate::{
    config::{Environment, Section},
    fake_bao::FakeBao,
    secret::Secret,
};

/// One push as the recorder saw it: the environment, and each key with its
/// value in the clear, which only a test may hold.
type Push = (String, Vec<(String, String)>);

/// A shepherd that records every push, and refuses them while `refuse` is set.
#[derive(Debug, Default)]
struct Recorder {
    pushes: RefCell<Vec<Push>>,
    refuse: Cell<bool>,
}

impl Shepherd for Rc<Recorder> {
    async fn push(&self, environment: &str, set: &Set) -> Result<u32, RequestError> {
        if self.refuse.get() {
            return Err(RequestError::Closed);
        }
        let entries: Vec<(String, String)> = set
            .iter()
            .map(|(key, value)| (key.clone(), value.expose().to_string()))
            .collect();
        let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        self.pushes
            .borrow_mut()
            .push((environment.to_string(), entries));
        Ok(count)
    }
}

impl Recorder {
    fn count(&self) -> usize {
        self.pushes.borrow().len()
    }

    fn last(&self) -> Push {
        self.pushes.borrow().last().cloned().expect("a push")
    }
}

const LOGIN: &str = "/v1/auth/approle/login";

fn config(fake: &FakeBao, environments: &[(&str, &[&str])]) -> Config {
    let mut text = format!(
        "address = \"{}\"\nrole_id = \"role\"\nsecret_id = \"hunter2\"\n",
        fake.address()
    );
    for (name, paths) in environments {
        let quoted: Vec<String> = paths.iter().map(|p| format!("\"{p}\"")).collect();
        text.push_str(&format!(
            "[environments.{name}]\npaths = [{}]\n",
            quoted.join(", ")
        ));
    }
    shep_client::dogs::parse_section::<Section>("openbao", &text)
        .expect("parses")
        .resolve()
        .expect("resolves")
}

async fn setup(environments: &[(&str, &[&str])]) -> (FakeBao, Rc<Recorder>, Mirror<Rc<Recorder>>) {
    let fake = FakeBao::start().await;
    fake.login_ok("s.token", 3600);
    let recorder = Rc::new(Recorder::default());
    let mirror = Mirror::new(Rc::clone(&recorder), config(&fake, environments)).expect("a mirror");
    (fake, recorder, mirror)
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[tokio::test]
async fn the_first_round_pushes_every_environment_with_its_values() {
    let (fake, recorder, mut mirror) = setup(&[
        ("production", &["app/prod", "shared"]),
        ("staging", &["app/staging"]),
    ])
    .await;
    fake.kv("secret", "app/prod", r#"{"DB_URL": "postgres://prod"}"#);
    fake.kv("secret", "shared", r#"{"SENTRY_DSN": "https://sentry"}"#);
    fake.kv(
        "secret",
        "app/staging",
        r#"{"DB_URL": "postgres://staging"}"#,
    );

    let outcomes = mirror.round(false).await;

    assert_eq!(
        outcomes,
        [
            Outcome::Pushed {
                environment: "production".to_string(),
                keys: vec!["DB_URL".to_string(), "SENTRY_DSN".to_string()],
            },
            Outcome::Pushed {
                environment: "staging".to_string(),
                keys: vec!["DB_URL".to_string()],
            },
        ]
    );
    assert_eq!(
        recorder.pushes.borrow()[0],
        (
            "production".to_string(),
            pairs(&[
                ("DB_URL", "postgres://prod"),
                ("SENTRY_DSN", "https://sentry")
            ])
        )
    );
    assert_eq!(fake.count(LOGIN), 1, "one login for the whole round");
}

#[tokio::test]
async fn an_unchanged_set_is_not_pushed_again_unless_forced() {
    let (fake, recorder, mut mirror) = setup(&[("production", &["app"])]).await;
    fake.kv("secret", "app", r#"{"A": "1"}"#);

    mirror.round(false).await;
    let again = mirror.round(false).await;
    assert_eq!(
        again,
        [Outcome::Unchanged {
            environment: "production".to_string()
        }]
    );
    assert_eq!(recorder.count(), 1);

    let forced = mirror.round(true).await;
    assert!(matches!(forced[0], Outcome::Pushed { .. }), "{forced:?}");
    assert_eq!(recorder.count(), 2);
}

#[tokio::test]
async fn a_changed_or_deleted_key_is_pushed_as_the_whole_new_set() {
    let (fake, recorder, mut mirror) = setup(&[("production", &["app"])]).await;
    fake.kv("secret", "app", r#"{"A": "1", "B": "2"}"#);
    mirror.round(false).await;

    fake.kv("secret", "app", r#"{"A": "changed"}"#);
    mirror.round(false).await;

    assert_eq!(
        recorder.last(),
        ("production".to_string(), pairs(&[("A", "changed")]))
    );
}

#[tokio::test]
async fn a_403_logs_in_once_more_then_skips_only_that_environment() {
    let (fake, recorder, mut mirror) =
        setup(&[("production", &["denied"]), ("staging", &["fine"])]).await;
    fake.answer(
        "GET",
        "/v1/secret/data/denied",
        403,
        r#"{"errors": ["permission denied"]}"#,
    );
    fake.kv("secret", "fine", r#"{"A": "1"}"#);

    let outcomes = mirror.round(false).await;

    assert_eq!(
        outcomes[0],
        Outcome::Skipped {
            environment: "production".to_string(),
            reasons: vec!["reading secret/denied: permission denied (403)".to_string()],
        }
    );
    assert!(
        matches!(outcomes[1], Outcome::Pushed { .. }),
        "{outcomes:?}"
    );
    assert_eq!(
        fake.count("/v1/secret/data/denied"),
        2,
        "read once more after logging in"
    );
    assert_eq!(
        fake.count(LOGIN),
        2,
        "the round's login and one more after the 403"
    );
    assert_eq!(recorder.count(), 1);
}

#[tokio::test]
async fn a_failed_read_keeps_the_last_push_in_place() {
    let (fake, recorder, mut mirror) = setup(&[("production", &["app", "shared"])]).await;
    fake.kv("secret", "app", r#"{"A": "1"}"#);
    fake.kv("secret", "shared", r#"{"B": "2"}"#);
    mirror.round(false).await;

    fake.answer(
        "GET",
        "/v1/secret/data/shared",
        503,
        r#"{"errors": ["sealed"]}"#,
    );
    let outcomes = mirror.round(false).await;

    assert_eq!(
        outcomes,
        [Outcome::Skipped {
            environment: "production".to_string(),
            reasons: vec![
                "reading secret/shared: OpenBao answered 503 Service Unavailable".to_string()
            ],
        }]
    );
    assert_eq!(recorder.count(), 1, "nothing replaced the last push");
}

#[tokio::test]
async fn a_key_at_two_paths_skips_the_push_and_names_both() {
    let (fake, recorder, mut mirror) = setup(&[("production", &["app", "shared"])]).await;
    fake.kv("secret", "app", r#"{"DB_URL": "a"}"#);
    fake.kv("secret", "shared", r#"{"DB_URL": "b"}"#);

    let outcomes = mirror.round(false).await;

    assert_eq!(
        outcomes[0].to_string(),
        "production: kept the last push: key `DB_URL` is at both secret/app and secret/shared"
    );
    assert_eq!(recorder.count(), 0);
}

#[tokio::test]
async fn a_refused_login_skips_every_environment_after_one_attempt() {
    let (fake, recorder, mut mirror) = setup(&[("production", &["a"]), ("staging", &["b"])]).await;
    fake.answer(
        "POST",
        LOGIN,
        400,
        r#"{"errors": ["invalid role or secret ID"]}"#,
    );

    let outcomes = mirror.round(false).await;

    let reason = "logging in to OpenBao: OpenBao answered 400 Bad Request".to_string();
    assert_eq!(
        outcomes,
        [
            Outcome::Skipped {
                environment: "production".to_string(),
                reasons: vec![reason.clone()],
            },
            Outcome::Skipped {
                environment: "staging".to_string(),
                reasons: vec![reason],
            },
        ]
    );
    assert_eq!(fake.count(LOGIN), 1);
    assert_eq!(recorder.count(), 0);
}

#[tokio::test]
async fn a_refused_push_is_tried_again_next_round() {
    let (fake, recorder, mut mirror) = setup(&[("production", &["app"])]).await;
    fake.kv("secret", "app", r#"{"A": "1"}"#);
    recorder.refuse.set(true);

    let refused = mirror.round(false).await;
    assert!(matches!(refused[0], Outcome::Refused { .. }), "{refused:?}");

    recorder.refuse.set(false);
    let retried = mirror.round(false).await;
    assert!(matches!(retried[0], Outcome::Pushed { .. }), "{retried:?}");
}

#[tokio::test]
async fn an_environment_that_leaves_the_config_is_emptied_once() {
    let (fake, recorder, mut mirror) =
        setup(&[("production", &["app"]), ("staging", &["app"])]).await;
    fake.kv("secret", "app", r#"{"A": "1"}"#);
    mirror.round(false).await;

    let outcomes = mirror
        .reconfigure(config(&fake, &[("production", &["app"])]))
        .await
        .expect("reconfigures");

    assert_eq!(
        outcomes,
        [Outcome::Emptied {
            environment: "staging".to_string()
        }]
    );
    assert_eq!(recorder.last(), ("staging".to_string(), Vec::new()));
    let next = mirror.round(false).await;
    assert_eq!(
        next,
        [Outcome::Unchanged {
            environment: "production".to_string()
        }]
    );
}

#[tokio::test]
async fn new_login_fields_log_in_again_and_new_paths_push_next_round() {
    let (fake, recorder, mut mirror) = setup(&[("production", &["app"])]).await;
    fake.kv("secret", "app", r#"{"A": "1"}"#);
    fake.kv("secret", "more", r#"{"B": "2"}"#);
    mirror.round(false).await;

    let mut changed = config(&fake, &[("production", &["app", "more"])]);
    changed.secret_id = Secret::new("rotated".to_string());
    mirror.reconfigure(changed).await.expect("reconfigures");
    mirror.round(false).await;

    assert_eq!(fake.count(LOGIN), 2);
    let login = fake
        .seen()
        .into_iter()
        .rfind(|s| s.path == LOGIN)
        .expect("a login");
    assert!(login.body.contains("rotated"));
    assert_eq!(
        recorder.last(),
        ("production".to_string(), pairs(&[("A", "1"), ("B", "2")]))
    );
}

#[tokio::test]
async fn a_client_that_cannot_be_rebuilt_leaves_the_old_settings_in_force() {
    let (fake, _recorder, mut mirror) = setup(&[("production", &["app"])]).await;
    let mut broken = config(&fake, &[]);
    broken.ca_cert = Some(std::path::PathBuf::from("/nonexistent/ca.pem"));

    assert!(mirror.reconfigure(broken).await.is_err());
    assert_eq!(mirror.config().environments.len(), 1);
    assert_eq!(
        mirror.config().environments["production"],
        Environment {
            mount: "secret".to_string(),
            paths: vec!["app".to_string()],
        }
    );
}

#[test]
fn outcomes_read_as_one_line_each() {
    let pushed = Outcome::Pushed {
        environment: "production".to_string(),
        keys: vec!["A".to_string(), "B".to_string()],
    };
    assert_eq!(pushed.to_string(), "production: pushed 2 secrets: A, B");
    let one = Outcome::Pushed {
        environment: "production".to_string(),
        keys: vec!["A".to_string()],
    };
    assert_eq!(one.to_string(), "production: pushed 1 secret: A");
    let empty = Outcome::Pushed {
        environment: "production".to_string(),
        keys: Vec::new(),
    };
    assert_eq!(empty.to_string(), "production: pushed an empty set");
    assert!(
        !Outcome::Unchanged {
            environment: "p".to_string()
        }
        .worth_logging()
    );
    assert!(pushed.worth_logging());
}
