//! [`Secret`], the one type a credential or a mirrored value lives in.
//!
//! Everything this dog handles that must not be printed goes through here:
//! the AppRole secret ID, the token a login returns, and every value read out
//! of a KV path. There is no `Display`, and `Debug` never reads the value, so
//! a `{:?}` of any struct holding one, or of an error chain that reached one,
//! cannot show it.

use core::fmt;

use serde::Deserialize;
use shep_client::shep_core::protocol::EnvValue;

/// A string that is never printed.
///
/// `PartialEq` compares the values. It exists so a pushed set can be compared
/// with the last one to decide whether anything changed, never to check a
/// credential, so there is no timing-safe comparison to protect here.
///
/// `Deserialize` is transparent, so a config field can be one from the moment
/// it is parsed and a derived `Debug` on the struct holding it stays safe.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Wraps `value`.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The value itself, for the one place that has to send it somewhere:
    /// an HTTP body or header, or a push.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The value as a push entry, which redacts its own `Debug` the same way.
    #[must_use]
    pub fn into_env_value(self) -> EnvValue {
        EnvValue::from(self.0)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Written out rather than through `str`'s `Debug`, which would quote
        // it and make the placeholder look like a value in an error chain.
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact string, not an absence check: a `Debug` that printed nothing at
    /// all, or printed the length, would pass a test that only looked for
    /// the value being missing.
    #[test]
    fn debug_prints_the_placeholder_and_never_the_value() {
        let secret = Secret::new("hunter2".to_string());
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{:?}", Some(&secret)), "Some(<redacted>)");
    }

    #[test]
    fn a_push_entry_carries_the_value_and_still_redacts_it() {
        let value = Secret::new("hunter2".to_string()).into_env_value();
        assert_eq!(value.as_str(), "hunter2");
        assert!(!format!("{value:?}").contains("hunter2"));
    }
}
