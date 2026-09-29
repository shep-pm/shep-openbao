//! The one thing this dog asks of its shepherd: take a push.
//!
//! Behind a trait so the run loop can be tested against a recorder. shep's
//! own fake daemon answers every `PutSecrets` with a `Pong` and keeps
//! nothing, which cannot say what was pushed.

use core::future::Future;

use shep_client::{
    ReconnectingClient, RequestError,
    shep_core::protocol::{Request, Response},
};

use crate::mirror::Set;

/// Where a push goes.
pub trait Shepherd {
    /// Replaces this dog's namespace for `environment` with `set`, and says
    /// how many entries the shepherd now holds for it.
    ///
    /// # Errors
    ///
    /// [`RequestError`], including [`RequestError::UnexpectedReply`] for an
    /// answer that is not `SecretsPut`.
    fn push(&self, environment: &str, set: &Set)
    -> impl Future<Output = Result<u32, RequestError>>;
}

/// A shepherd over a live connection, pushing under `namespace`.
#[derive(Debug)]
pub struct Link<'a> {
    client: &'a ReconnectingClient,
    namespace: String,
}

impl<'a> Link<'a> {
    /// Pushes through `client` under `namespace`, which shep expects to be
    /// the dog's registered name.
    #[must_use]
    pub fn new(client: &'a ReconnectingClient, namespace: String) -> Self {
        Self { client, namespace }
    }
}

impl Shepherd for Link<'_> {
    async fn push(&self, environment: &str, set: &Set) -> Result<u32, RequestError> {
        let entries = set
            .iter()
            .map(|(key, value)| (key.clone(), value.clone().into_env_value()))
            .collect();
        let request = Request::PutSecrets {
            namespace: self.namespace.clone(),
            environment: environment.to_string(),
            entries,
        };
        match self.client.request(request).await? {
            Response::SecretsPut { accepted } => Ok(accepted),
            other => Err(RequestError::UnexpectedReply {
                asked: "PutSecrets",
                answered: other.name(),
            }),
        }
    }
}
