//! OpenBao as its root token sees it: what a test sets up for the dog to read.

use serde_json::{Value, json};

use super::required;

/// OpenBao as its root token sees it, for setting up what the dog reads.
pub struct Bao {
    pub address: String,
    token: String,
    http: reqwest::Client,
    runtime: tokio::runtime::Runtime,
}

/// What a test's AppRole logs in with.
pub struct Login {
    pub role_id: String,
    pub secret_id: String,
}

impl Bao {
    pub fn new() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            address: required("BAO_ADDR", "http://127.0.0.1:18200")
                .trim_end_matches('/')
                .to_string(),
            token: std::env::var("BAO_TOKEN").unwrap_or_else(|_| "root".to_string()),
            http: reqwest::Client::new(),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime"),
        }
    }

    /// One call as root, answering the status and the JSON body (`null` for
    /// an empty one).
    fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let url = format!("{}/v1/{path}", self.address);
        self.runtime.block_on(async {
            let mut request = self
                .http
                .request(method, url)
                .header("X-Vault-Token", &self.token);
            if let Some(body) = body {
                request = request
                    .header("Content-Type", "application/json")
                    .body(body.to_string());
            }
            let response = request.send().await.expect("OpenBao answered");
            let status = response.status().as_u16();
            let text = response.text().await.expect("a body");
            let json = if text.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).expect("a JSON body")
            };
            (status, json)
        })
    }

    fn ok(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let (status, json) = self.call(method, path, body);
        assert!(
            (200..300).contains(&status),
            "{path} answered {status}: {json}"
        );
        json
    }

    /// An AppRole that may read everything under `secret/<prefix>/`, and
    /// nothing else.
    pub fn approle(&self, prefix: &str) -> Login {
        // 400 once it is enabled, which a second test in the same run finds.
        let _ = self.call(
            reqwest::Method::POST,
            "sys/auth/approle",
            Some(json!({"type": "approle"})),
        );
        let policy = format!("path \"secret/data/{prefix}/*\" {{ capabilities = [\"read\"] }}");
        self.ok(
            reqwest::Method::PUT,
            &format!("sys/policies/acl/{prefix}"),
            Some(json!({ "policy": policy })),
        );
        self.ok(
            reqwest::Method::POST,
            &format!("auth/approle/role/{prefix}"),
            Some(json!({"token_policies": [prefix], "token_ttl": "1h", "secret_id_num_uses": 0})),
        );
        let role = self.ok(
            reqwest::Method::GET,
            &format!("auth/approle/role/{prefix}/role-id"),
            None,
        );
        let secret = self.ok(
            reqwest::Method::POST,
            &format!("auth/approle/role/{prefix}/secret-id"),
            None,
        );
        Login {
            role_id: role["data"]["role_id"]
                .as_str()
                .expect("a role ID")
                .to_string(),
            secret_id: secret["data"]["secret_id"]
                .as_str()
                .expect("a secret ID")
                .to_string(),
        }
    }

    /// Writes a new version of `secret/<path>` holding exactly `data`.
    pub fn put(&self, path: &str, data: Value) {
        self.ok(
            reqwest::Method::POST,
            &format!("secret/data/{path}"),
            Some(json!({ "data": data })),
        );
    }
}
