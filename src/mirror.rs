//! Building one environment's set of secrets from what its KV paths hold.
//!
//! A push replaces everything the namespace held for that environment, so a
//! set built from a partial or conflicting read would delete secrets a sheep
//! depends on. Every problem found here refuses the whole set, and every one
//! is reported, not only the first, so an operator fixes them in one pass.

use core::fmt;
use std::collections::BTreeMap;

use shep_client::shep_core::secrets::{MAX_VALUE_BYTES, is_name};

use crate::{bao::KvValue, secret::Secret};

/// One environment's secrets, keyed by name: what a push carries.
pub type Set = BTreeMap<String, Secret>;

/// Why a key cannot go into the set. Names keys and paths, never values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The same key at two paths. Neither wins: which one the operator
    /// meant is theirs to say.
    Collision {
        key: String,
        first: String,
        second: String,
    },
    /// A key shep refuses as a secret name, so no `{{secret:...}}`
    /// reference could reach it and shep would refuse the whole push.
    BadName { key: String, path: String },
    /// `null`, an array or an object, which have no environment form.
    Unsupported { key: String, path: String },
    /// Longer than shep accepts for one value.
    TooLarge { key: String, path: String },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collision { key, first, second } => {
                write!(f, "key `{key}` is at both {first} and {second}")
            }
            Self::BadName { key, path } => write!(
                f,
                "key `{key}` at {path} is not a name shep accepts: use letters, digits, `.`, `_` and `-`, not starting with `.`"
            ),
            Self::Unsupported { key, path } => write!(
                f,
                "key `{key}` at {path} is null, a list or a table, not a string, number or boolean"
            ),
            Self::TooLarge { key, path } => write!(
                f,
                "key `{key}` at {path} is over shep's {MAX_VALUE_BYTES} byte limit"
            ),
        }
    }
}

/// Merges what each path held into one set, `reads` in the order the paths
/// are configured, each labelled as `mount/path`.
///
/// # Errors
///
/// Every [`Problem`] found, in the order found, when there is at least one.
pub fn build(reads: Vec<(String, BTreeMap<String, KvValue>)>) -> Result<Set, Vec<Problem>> {
    let mut set = Set::new();
    let mut from: BTreeMap<String, String> = BTreeMap::new();
    let mut problems = Vec::new();
    for (path, values) in reads {
        for (key, value) in values {
            if let Some(first) = from.get(&key) {
                problems.push(Problem::Collision {
                    key,
                    first: first.clone(),
                    second: path.clone(),
                });
                continue;
            }
            from.insert(key.clone(), path.clone());
            if !is_name(&key) {
                problems.push(Problem::BadName {
                    key,
                    path: path.clone(),
                });
                continue;
            }
            match value {
                KvValue::Unsupported => problems.push(Problem::Unsupported {
                    key,
                    path: path.clone(),
                }),
                KvValue::Text(text) if text.expose().len() > MAX_VALUE_BYTES => {
                    problems.push(Problem::TooLarge {
                        key,
                        path: path.clone(),
                    });
                }
                KvValue::Text(text) => {
                    set.insert(key, text);
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(set)
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> KvValue {
        KvValue::Text(Secret::new(value.to_string()))
    }

    fn read(path: &str, pairs: &[(&str, KvValue)]) -> (String, BTreeMap<String, KvValue>) {
        let values = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect();
        (path.to_string(), values)
    }

    #[test]
    fn keys_from_every_path_merge_into_one_set() {
        let set = build(vec![
            read("secret/app", &[("DB_URL", text("postgres://db"))]),
            read("secret/shared", &[("SENTRY_DSN", text("https://sentry"))]),
        ])
        .expect("no problems");
        assert_eq!(set.len(), 2);
        assert_eq!(set["DB_URL"].expose(), "postgres://db");
        assert_eq!(set["SENTRY_DSN"].expose(), "https://sentry");
    }

    #[test]
    fn nothing_anywhere_is_an_empty_set_not_a_problem() {
        assert_eq!(build(vec![read("secret/app", &[])]), Ok(Set::new()));
    }

    #[test]
    fn a_key_at_two_paths_names_both_and_takes_neither() {
        let problems = build(vec![
            read("secret/app", &[("DB_URL", text("a"))]),
            read("secret/shared", &[("DB_URL", text("b"))]),
        ])
        .expect_err("a collision");
        assert_eq!(
            problems,
            [Problem::Collision {
                key: "DB_URL".to_string(),
                first: "secret/app".to_string(),
                second: "secret/shared".to_string(),
            }]
        );
        assert_eq!(
            problems[0].to_string(),
            "key `DB_URL` is at both secret/app and secret/shared"
        );
    }

    #[test]
    fn every_problem_is_reported_in_one_pass() {
        let problems = build(vec![read(
            "secret/app",
            &[
                ("db url", text("a")),
                ("LIST", KvValue::Unsupported),
                ("BIG", text(&"x".repeat(MAX_VALUE_BYTES + 1))),
                ("FINE", text("ok")),
            ],
        )])
        .expect_err("three problems");
        let path = "secret/app".to_string();
        assert_eq!(
            problems,
            [
                Problem::TooLarge {
                    key: "BIG".to_string(),
                    path: path.clone()
                },
                Problem::Unsupported {
                    key: "LIST".to_string(),
                    path: path.clone()
                },
                Problem::BadName {
                    key: "db url".to_string(),
                    path
                },
            ]
        );
    }

    #[test]
    fn a_value_at_exactly_the_limit_is_kept() {
        let set = build(vec![read(
            "secret/app",
            &[("BIG", text(&"x".repeat(MAX_VALUE_BYTES)))],
        )])
        .expect("at the limit is fine");
        assert_eq!(set["BIG"].expose().len(), MAX_VALUE_BYTES);
    }

    #[test]
    fn a_problem_never_prints_a_value() {
        let problems = build(vec![read(
            "secret/app",
            &[(
                "BIG",
                text(&format!("hunter2{}", "x".repeat(MAX_VALUE_BYTES))),
            )],
        )])
        .expect_err("too large");
        assert_eq!(
            problems[0].to_string(),
            format!("key `BIG` at secret/app is over shep's {MAX_VALUE_BYTES} byte limit")
        );
        assert_eq!(
            format!("{:?}", problems[0]),
            "TooLarge { key: \"BIG\", path: \"secret/app\" }"
        );
    }
}
