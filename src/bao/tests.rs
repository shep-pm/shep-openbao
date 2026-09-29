use super::*;
use crate::fake_bao::FakeBao;

fn client(fake: &FakeBao, namespace: Option<&str>) -> Bao {
    Bao::new(fake.address(), None, namespace.map(str::to_string)).expect("a client")
}

fn secret(value: &str) -> Secret {
    Secret::new(value.to_string())
}

async fn logged_in(fake: &FakeBao, bao: &Bao) -> Token {
    fake.login_ok("s.token", 3600);
    bao.login("role", &secret("hunter2"))
        .await
        .expect("logs in")
}

#[tokio::test]
async fn login_posts_the_role_and_secret_id_and_keeps_the_token() {
    let fake = FakeBao::start().await;
    let bao = client(&fake, None);
    let token = logged_in(&fake, &bao).await;

    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, "/v1/auth/approle/login");
    let body: serde_json::Value = serde_json::from_str(&seen[0].body).expect("a JSON body");
    assert_eq!(
        body,
        serde_json::json!({"role_id": "role", "secret_id": "hunter2"})
    );
    assert_eq!(token.value.expose(), "s.token");
    assert_eq!(token.lifetime, Some(Duration::from_secs(3600)));
}

#[tokio::test]
async fn a_read_sends_the_token_and_namespace_and_decodes_the_values() {
    let fake = FakeBao::start().await;
    let bao = client(&fake, Some("team-a"));
    let token = logged_in(&fake, &bao).await;
    fake.kv(
        "kv/apps",
        "myapp/prod",
        r#"{"DB_URL": "postgres://db", "PORT": 5432}"#,
    );

    let values = bao
        .read(&token, "kv/apps", "myapp/prod")
        .await
        .expect("reads");

    assert_eq!(values["DB_URL"], KvValue::Text(secret("postgres://db")));
    assert_eq!(values["PORT"], KvValue::Text(secret("5432")));
    let read = fake.seen().pop().expect("a read");
    assert_eq!(read.method, "GET");
    assert_eq!(read.path, "/v1/kv/apps/data/myapp/prod");
    assert_eq!(read.headers["x-vault-token"], "s.token");
    assert_eq!(read.headers["x-vault-namespace"], "team-a");
}

#[tokio::test]
async fn no_namespace_header_goes_out_without_a_namespace() {
    let fake = FakeBao::start().await;
    let bao = client(&fake, None);
    logged_in(&fake, &bao).await;
    assert!(!fake.seen()[0].headers.contains_key("x-vault-namespace"));
}

#[tokio::test]
async fn each_path_segment_is_encoded_on_its_own() {
    let fake = FakeBao::start().await;
    let bao = client(&fake, None);
    let token = logged_in(&fake, &bao).await;
    fake.kv("secret", "my%20app/prod%3Fx", r#"{}"#);

    bao.read(&token, "secret", "my app/prod?x")
        .await
        .expect("reads");

    assert_eq!(
        fake.seen().pop().expect("a read").path,
        "/v1/secret/data/my%20app/prod%3Fx"
    );
}

#[tokio::test]
async fn statuses_map_to_their_own_errors() {
    let fake = FakeBao::start().await;
    let bao = client(&fake, None);
    let token = logged_in(&fake, &bao).await;
    fake.answer(
        "GET",
        "/v1/secret/data/denied",
        403,
        r#"{"errors": ["permission denied"]}"#,
    );
    fake.answer(
        "GET",
        "/v1/secret/data/sealed",
        503,
        r#"{"errors": ["Vault is sealed"]}"#,
    );

    let denied = bao.read(&token, "secret", "denied").await.expect_err("403");
    let missing = bao
        .read(&token, "secret", "missing")
        .await
        .expect_err("404");
    let sealed = bao.read(&token, "secret", "sealed").await.expect_err("503");

    assert!(matches!(denied, BaoError::Forbidden(_)), "{denied:?}");
    assert!(matches!(missing, BaoError::NotFound(_)), "{missing:?}");
    assert!(
        matches!(sealed, BaoError::Status { status, .. } if status == StatusCode::SERVICE_UNAVAILABLE),
        "{sealed:?}"
    );
}

/// Exact strings, so a message that grew a body or a value fails here
/// rather than passing an absence check.
#[tokio::test]
async fn error_messages_name_the_operation_and_never_the_body() {
    let fake = FakeBao::start().await;
    let bao = client(&fake, None);
    fake.answer(
        "POST",
        "/v1/auth/approle/login",
        400,
        r#"{"errors": ["invalid role or secret ID"]}"#,
    );
    let login = bao
        .login("role", &secret("hunter2"))
        .await
        .expect_err("400");
    assert_eq!(
        login.to_string(),
        "logging in to OpenBao: OpenBao answered 400 Bad Request"
    );

    fake.login_ok("s.token", 0);
    let token = bao
        .login("role", &secret("hunter2"))
        .await
        .expect("logs in");
    fake.answer(
        "GET",
        "/v1/secret/data/odd",
        200,
        r#"{"data": {"data": "hunter2"}}"#,
    );
    let odd = bao
        .read(&token, "secret", "odd")
        .await
        .expect_err("an odd body");
    assert_eq!(
        odd.to_string(),
        "reading secret/odd: OpenBao's answer is not the shape its API documents"
    );
    assert_eq!(
        format!("{odd:?}"),
        "Decode(Read { mount: \"secret\", path: \"odd\" })"
    );
    let missing = bao.read(&token, "secret", "gone").await.expect_err("404");
    assert_eq!(
        missing.to_string(),
        "reading secret/gone: nothing there (404)"
    );
}

#[tokio::test]
async fn an_answer_over_the_cap_is_refused_unread() {
    let fake = FakeBao::start().await;
    let bao = client(&fake, None);
    let token = logged_in(&fake, &bao).await;
    let big = format!(r#"{{"A": "{}"}}"#, "x".repeat(MAX_RESPONSE_BYTES));
    fake.kv("secret", "big", &big);

    let err = bao
        .read(&token, "secret", "big")
        .await
        .expect_err("too large");

    assert!(matches!(err, BaoError::TooLarge(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        "reading secret/big: OpenBao's answer is over 4 MiB, which this dog does not read"
    );
}

#[tokio::test]
async fn a_refused_connection_is_a_transport_error() {
    let fake = FakeBao::start().await;
    let mut address = fake.address();
    // Port 9 (discard) on loopback: nothing listens there in a test run.
    address.set_port(Some(9)).expect("a port");
    let bao = Bao::new(address, None, None).expect("a client");
    let err = bao
        .login("role", &secret("hunter2"))
        .await
        .expect_err("refused");
    assert_eq!(
        err.to_string(),
        "logging in to OpenBao: no answer from OpenBao"
    );
    assert!(core::error::Error::source(&err).is_some());
}

#[test]
fn a_token_is_due_two_thirds_through_its_lifetime_and_never_without_one() {
    let issued = Instant::now();
    let token = Token {
        value: secret("s.token"),
        lifetime: Some(Duration::from_secs(90)),
        issued,
    };
    assert!(!token.due(issued + Duration::from_secs(59)));
    assert!(token.due(issued + Duration::from_secs(60)));
    let forever = Token {
        value: secret("s.root"),
        lifetime: None,
        issued,
    };
    assert!(!forever.due(issued + Duration::from_secs(1_000_000)));
    assert_eq!(
        format!("{token:?}").split(", issued").next(),
        Some("Token { value: <redacted>, lifetime: Some(90s)")
    );
}

#[test]
fn a_lifetime_too_large_to_double_does_not_panic() {
    let issued = Instant::now();
    let token = Token {
        value: secret("s.token"),
        lifetime: Some(Duration::from_secs(u64::MAX)),
        issued,
    };
    assert!(!token.due(issued + Duration::from_secs(3600)));
}

#[test]
fn a_missing_or_empty_ca_cert_is_refused_by_path() {
    let url = Url::parse("https://openbao.example.com").expect("a URL");
    let dir = std::env::temp_dir().join(format!("shep-openbao-ca-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let empty = dir.join("empty.pem");
    std::fs::write(&empty, "not a certificate").expect("written");

    let missing = Bao::new(url.clone(), Some(&dir.join("nope.pem")), None).expect_err("missing");
    let unusable = Bao::new(url, Some(&empty), None).expect_err("not PEM");

    assert!(
        matches!(missing, BaoError::CaCertRead { .. }),
        "{missing:?}"
    );
    assert!(
        matches!(unusable, BaoError::CaCertParse { .. }),
        "{unusable:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
