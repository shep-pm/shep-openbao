//! [`Bao`], the two calls this dog makes to OpenBao: an AppRole login and a
//! KV v2 read.
//!
//! Written against `reqwest` rather than taken from a Vault client crate so
//! that every type touching a credential or a value is this crate's own and
//! redacts from the start (ADR-0001's companion decision, recorded in issue
//! #3). No error here carries a response body: OpenBao's bodies are either
//! the values themselves or messages this dog has no need to repeat, so an
//! error names the operation and the status and stops.

mod value;

use core::{fmt, time::Duration};
use std::{collections::BTreeMap, path::Path, time::Instant};

use reqwest::{StatusCode, Url};

pub use value::KvValue;
use value::{LoginResponse, ReadResponse};

use crate::secret::Secret;

/// The longest any one request may take, connect included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest a TCP and TLS connect may take on its own.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The header OpenBao reads a token from.
const TOKEN_HEADER: &str = "X-Vault-Token";

/// The header OpenBao reads a namespace from. `Vault` in the name is
/// OpenBao's own spelling, kept for compatibility.
const NAMESPACE_HEADER: &str = "X-Vault-Namespace";

/// A client for one OpenBao server.
#[derive(Debug)]
pub struct Bao {
    http: reqwest::Client,
    address: Url,
    namespace: Option<String>,
}

/// A logged-in session.
#[derive(Debug)]
pub struct Token {
    value: Secret,
    /// `None` for a token OpenBao issued with no expiry.
    lifetime: Option<Duration>,
    issued: Instant,
}

impl Token {
    /// Whether it is time to log in again: two thirds of the way through the
    /// token's lifetime, so a round that starts just before expiry does not
    /// run into it halfway through.
    #[must_use]
    pub fn due(&self, now: Instant) -> bool {
        self.lifetime
            .is_some_and(|lifetime| now.saturating_duration_since(self.issued) >= lifetime * 2 / 3)
    }
}

/// What a request was for, so an error can say which one failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// `auth/approle/login`.
    Login,
    /// A KV v2 read of `path` under `mount`.
    Read { mount: String, path: String },
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Login => f.write_str("logging in to OpenBao"),
            Self::Read { mount, path } => write!(f, "reading {mount}/{path}"),
        }
    }
}

/// Why a call to OpenBao failed.
#[derive(Debug)]
pub enum BaoError {
    /// `ca_cert` could not be read.
    CaCertRead {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    /// `ca_cert` holds no certificate this dog can use.
    CaCertParse { path: std::path::PathBuf },
    /// The HTTP client could not be built, usually over the TLS setup.
    Client(reqwest::Error),
    /// No answer: a refused connection, a TLS failure or a timeout.
    Transport {
        op: Operation,
        source: reqwest::Error,
    },
    /// OpenBao answered 403: the token is no longer valid, or its policy does
    /// not reach this path.
    Forbidden(Operation),
    /// OpenBao answered 404: nothing at this path, or its latest version is
    /// deleted.
    NotFound(Operation),
    /// Any other status that is not a success, such as 503 for a sealed
    /// server.
    Status { op: Operation, status: StatusCode },
    /// A success whose body is not what OpenBao's API documents.
    Decode(Operation),
}

impl fmt::Display for BaoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CaCertRead { path, .. } => write!(f, "cannot read ca_cert {}", path.display()),
            Self::CaCertParse { path } => {
                write!(f, "ca_cert {} holds no PEM certificate", path.display())
            }
            Self::Client(_) => f.write_str("cannot set up an HTTPS client"),
            Self::Transport { op, .. } => write!(f, "{op}: no answer from OpenBao"),
            Self::Forbidden(op) => write!(f, "{op}: permission denied (403)"),
            Self::NotFound(op) => write!(f, "{op}: nothing there (404)"),
            Self::Status { op, status } => write!(f, "{op}: OpenBao answered {status}"),
            Self::Decode(op) => write!(
                f,
                "{op}: OpenBao's answer is not the shape its API documents"
            ),
        }
    }
}

impl core::error::Error for BaoError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::CaCertRead { source, .. } => Some(source),
            Self::Client(source) | Self::Transport { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Bao {
    /// A client for the server at `address`, trusting `ca_cert` beside the
    /// system's own roots.
    ///
    /// # Errors
    ///
    /// - [`BaoError::CaCertRead`] and [`BaoError::CaCertParse`]: `ca_cert`
    ///   is unreadable or holds no certificate.
    /// - [`BaoError::Client`]: the TLS setup failed.
    pub fn new(
        address: Url,
        ca_cert: Option<&Path>,
        namespace: Option<String>,
    ) -> Result<Self, BaoError> {
        // reqwest's `rustls-no-provider` panics on the first client built
        // with no provider installed. An `Err` here only means one already
        // is, which is as good.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            // A redirect would carry the token header to wherever it points.
            .redirect(reqwest::redirect::Policy::none());
        if let Some(path) = ca_cert {
            let pem = std::fs::read(path).map_err(|source| BaoError::CaCertRead {
                path: path.to_path_buf(),
                source,
            })?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .ok()
                .filter(|certs| !certs.is_empty())
                .ok_or_else(|| BaoError::CaCertParse {
                    path: path.to_path_buf(),
                })?;
            builder = builder.tls_certs_merge(certs);
        }
        let http = builder.build().map_err(BaoError::Client)?;
        Ok(Self {
            http,
            address,
            namespace,
        })
    }

    /// Logs in with AppRole.
    ///
    /// # Errors
    ///
    /// Any [`BaoError`] but the `CaCert` and `Client` ones, with
    /// [`Operation::Login`].
    pub async fn login(&self, role_id: &str, secret_id: &Secret) -> Result<Token, BaoError> {
        let op = Operation::Login;
        let url = self.endpoint(&["auth", "approle", "login"]);
        let body = serde_json::json!({ "role_id": role_id, "secret_id": secret_id.expose() });
        let request = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        let bytes = self.send(&op, request).await?;
        let response: LoginResponse =
            serde_json::from_slice(&bytes).map_err(|_| BaoError::Decode(op))?;
        let lifetime = (response.auth.lease_duration > 0)
            .then(|| Duration::from_secs(response.auth.lease_duration));
        Ok(Token {
            value: response.auth.client_token,
            lifetime,
            issued: Instant::now(),
        })
    }

    /// Every key at `path` under the KV v2 `mount`, from its latest version.
    ///
    /// # Errors
    ///
    /// Any [`BaoError`] but the `CaCert` and `Client` ones, with
    /// [`Operation::Read`].
    pub async fn read(
        &self,
        token: &Token,
        mount: &str,
        path: &str,
    ) -> Result<BTreeMap<String, KvValue>, BaoError> {
        let op = Operation::Read {
            mount: mount.to_string(),
            path: path.to_string(),
        };
        let url = self.endpoint(&[mount, "data", path]);
        let request = self
            .http
            .get(url)
            .header(TOKEN_HEADER, token.value.expose());
        let bytes = self.send(&op, request).await?;
        let response: ReadResponse =
            serde_json::from_slice(&bytes).map_err(|_| BaoError::Decode(op))?;
        Ok(response.into_values())
    }

    /// `address` with `v1/` and each part appended, every `/`-separated
    /// segment percent-encoded on its own.
    fn endpoint(&self, parts: &[&str]) -> Url {
        let mut url = self.address.clone();
        // An `http` or `https` URL is always a base, which `Section::resolve`
        // guarantees, so the `Err` arm here never runs.
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.pop_if_empty().push("v1");
            for part in parts {
                segments.extend(part.split('/'));
            }
        }
        url
    }

    async fn send(
        &self,
        op: &Operation,
        mut request: reqwest::RequestBuilder,
    ) -> Result<Vec<u8>, BaoError> {
        if let Some(namespace) = &self.namespace {
            request = request.header(NAMESPACE_HEADER, namespace);
        }
        let transport = |source| BaoError::Transport {
            op: op.clone(),
            source,
        };
        let response = request.send().await.map_err(transport)?;
        let status = response.status();
        if status == StatusCode::FORBIDDEN {
            return Err(BaoError::Forbidden(op.clone()));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(BaoError::NotFound(op.clone()));
        }
        if !status.is_success() {
            return Err(BaoError::Status {
                op: op.clone(),
                status,
            });
        }
        let bytes = response.bytes().await.map_err(transport)?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests;
