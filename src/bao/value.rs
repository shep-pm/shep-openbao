//! The two response bodies this dog reads, decoded straight into [`Secret`]s.
//!
//! No intermediate type here holds a value it could print: a KV value is a
//! [`Secret`] from the moment serde builds it, and a login's token too. None
//! of these types derives `Debug`, so nothing can format one by accident.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::secret::Secret;

/// One value at a KV path, as this dog can hand it to shep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KvValue {
    /// A string, number or boolean, as the text a sheep's environment gets.
    /// A number keeps the digits OpenBao stored, and a boolean is `true` or
    /// `false`, the way shep's own `env` coerces them.
    Text(Secret),
    /// `null`, an array or an object, which have no single environment form.
    Unsupported,
}

/// `POST auth/approle/login`.
#[derive(Deserialize)]
pub(super) struct LoginResponse {
    pub(super) auth: LoginAuth,
}

#[derive(Deserialize)]
pub(super) struct LoginAuth {
    pub(super) client_token: Secret,
    /// Seconds until the token expires, and 0 for one that never does.
    pub(super) lease_duration: u64,
}

/// `GET <mount>/data/<path>` on a KV v2 mount.
#[derive(Deserialize)]
pub(super) struct ReadResponse {
    data: ReadData,
}

#[derive(Deserialize)]
struct ReadData {
    /// `None` when the latest version holds nothing, which OpenBao writes as
    /// `null` rather than as an empty object.
    ///
    /// Required all the same: `deserialize_with` stops serde treating a
    /// missing `Option` field as `None`. A 200 with no `data.data` at all is
    /// not a KV v2 answer, and reading it as an empty set would push one,
    /// deleting every secret the environment held.
    #[serde(deserialize_with = "Option::deserialize")]
    data: Option<BTreeMap<String, Raw>>,
}

/// Tried in order: a string first, so the common case never reaches the
/// others. Untagged, so a mismatch reports no content, only that no variant
/// fit.
#[derive(Deserialize)]
#[serde(untagged)]
enum Raw {
    Text(Secret),
    Bool(bool),
    Number(serde_json::Number),
    Other(IgnoredAny),
}

impl ReadResponse {
    /// Every key at the path with its value, in key order.
    pub(super) fn into_values(self) -> BTreeMap<String, KvValue> {
        self.data
            .data
            .unwrap_or_default()
            .into_iter()
            .map(|(key, raw)| {
                let value = match raw {
                    Raw::Text(text) => KvValue::Text(text),
                    Raw::Bool(flag) => KvValue::Text(Secret::new(flag.to_string())),
                    Raw::Number(number) => KvValue::Text(Secret::new(number.to_string())),
                    Raw::Other(IgnoredAny) => KvValue::Unsupported,
                };
                (key, value)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(body: &str) -> BTreeMap<String, KvValue> {
        serde_json::from_str::<ReadResponse>(body)
            .expect("the body decodes")
            .into_values()
    }

    fn text(value: &str) -> KvValue {
        KvValue::Text(Secret::new(value.to_string()))
    }

    #[test]
    fn scalars_become_text_and_the_rest_is_unsupported() {
        let got = values(
            r#"{"data": {"data": {
                "url": "postgres://db", "port": 5432, "ratio": 0.5, "debug": true,
                "nothing": null, "list": [1, 2], "nested": {"a": "b"}
            }, "metadata": {"version": 3}}}"#,
        );
        assert_eq!(got["url"], text("postgres://db"));
        assert_eq!(got["port"], text("5432"));
        assert_eq!(got["ratio"], text("0.5"));
        assert_eq!(got["debug"], text("true"));
        assert_eq!(got["nothing"], KvValue::Unsupported);
        assert_eq!(got["list"], KvValue::Unsupported);
        assert_eq!(got["nested"], KvValue::Unsupported);
    }

    #[test]
    fn a_null_version_is_an_empty_set() {
        assert!(values(r#"{"data": {"data": null, "metadata": {}}}"#).is_empty());
    }

    #[test]
    fn an_answer_with_no_data_at_all_is_refused_not_read_as_empty() {
        for body in [r#"{"data": {"metadata": {}}}"#, r#"{"data": {}}"#, r#"{}"#] {
            assert!(
                serde_json::from_str::<ReadResponse>(body).is_err(),
                "{body} should be refused"
            );
        }
    }

    /// serde's own message quotes the value it choked on. This is the reason
    /// `BaoError::Decode` drops it and names only the operation, pinned here
    /// so nobody "improves" that variant by attaching the source.
    #[test]
    fn serde_quotes_the_value_it_failed_on_which_is_why_decode_drops_it() {
        let Err(err) = serde_json::from_str::<ReadResponse>(r#"{"data": {"data": "hunter2"}}"#)
        else {
            panic!("a string where a map belongs is refused");
        };
        assert!(err.to_string().contains("hunter2"), "{err}");
    }
}
