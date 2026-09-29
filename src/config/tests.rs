use super::*;
use shep_client::dogs::parse_section;

const FULL: &str = r#"
    address = "https://openbao.example.com:8200"
    ca_cert = "/etc/ssl/openbao-ca.pem"
    openbao_namespace = "team-a"
    role_id = "role"
    secret_id = "hunter2"
    interval = "10m"
    persist = false

    [environments.production]
    mount = "kv/apps/"
    paths = ["/myapp/prod/", "shared/prod"]

    [environments.staging]
    paths = ["myapp/staging"]
"#;

const MINIMAL: &str = r#"
    address = "https://openbao.example.com:8200"
    role_id = "role"
    secret_id = "hunter2"
"#;

fn section(text: &str) -> Section {
    parse_section("openbao", text).expect("the section parses")
}

fn resolve(text: &str) -> Result<Config, ConfigError> {
    section(text).resolve()
}

fn with(extra: &str) -> Result<Config, ConfigError> {
    resolve(&format!("{MINIMAL}\n{extra}"))
}

fn with_address(address: &str) -> Result<Config, ConfigError> {
    resolve(&format!(
        "address = \"{address}\"\nrole_id = \"role\"\nsecret_id = \"hunter2\"\n"
    ))
}

#[test]
fn a_full_section_resolves_with_slashes_trimmed() {
    let config = resolve(FULL).expect("resolves");
    assert_eq!(config.address.as_str(), "https://openbao.example.com:8200/");
    assert_eq!(
        config.ca_cert,
        Some(PathBuf::from("/etc/ssl/openbao-ca.pem"))
    );
    assert_eq!(config.openbao_namespace.as_deref(), Some("team-a"));
    assert_eq!(config.role_id, "role");
    assert_eq!(config.secret_id.expose(), "hunter2");
    assert_eq!(config.interval, Duration::from_secs(600));
    assert!(!config.persist);
    assert_eq!(
        config.environments["production"],
        Environment {
            mount: "kv/apps".to_string(),
            paths: vec!["myapp/prod".to_string(), "shared/prod".to_string()],
        }
    );
    assert_eq!(config.environments["staging"].mount, "secret");
}

#[test]
fn defaults_fill_what_is_unset() {
    let config = resolve(MINIMAL).expect("resolves");
    assert_eq!(config.interval, DEFAULT_INTERVAL);
    assert_eq!(config.ca_cert, None);
    assert_eq!(config.openbao_namespace, None);
    assert!(config.persist);
    assert!(config.environments.is_empty());
}

#[test]
fn each_required_key_is_named_when_missing() {
    assert_eq!(resolve(""), Err(ConfigError::Missing("address")));
    assert_eq!(
        resolve("address = \"https://h\"\nsecret_id = \"s\""),
        Err(ConfigError::Missing("role_id"))
    );
    assert_eq!(
        resolve("address = \"https://h\"\nrole_id = \"r\""),
        Err(ConfigError::Missing("secret_id"))
    );
}

#[test]
fn empty_strings_are_refused_by_name() {
    assert_eq!(
        resolve("address = \"https://h\"\nrole_id = \"\"\nsecret_id = \"s\""),
        Err(ConfigError::Empty("role_id"))
    );
    assert_eq!(
        resolve("address = \"https://h\"\nrole_id = \"r\"\nsecret_id = \"\""),
        Err(ConfigError::Empty("secret_id"))
    );
    assert_eq!(
        with("openbao_namespace = \"\""),
        Err(ConfigError::Empty("openbao_namespace"))
    );
}

#[test]
fn http_is_accepted_only_to_a_loopback_address() {
    for fine in [
        "http://127.0.0.1:8200",
        "http://localhost:8200",
        "http://[::1]:8200",
    ] {
        assert!(with_address(fine).is_ok(), "{fine} should be accepted");
    }
    assert!(matches!(
        with_address("http://openbao.example.com:8200"),
        Err(ConfigError::Address(_))
    ));
    assert!(matches!(
        with_address("http://10.0.0.1:8200"),
        Err(ConfigError::Address(_))
    ));
}

#[test]
fn an_address_that_could_carry_a_credential_or_a_path_is_refused() {
    for bad in [
        "https://user:pass@openbao.example.com",
        "https://openbao.example.com/v1/",
        "https://openbao.example.com/?token=x",
        "ftp://openbao.example.com",
        "not a url",
    ] {
        assert!(
            matches!(with_address(bad), Err(ConfigError::Address(_))),
            "{bad} should be refused"
        );
    }
}

/// The parse error is dropped on purpose, so a value pasted into the
/// wrong key is never repeated back.
#[test]
fn an_address_error_never_quotes_the_address() {
    let err = with_address("hunter2").expect_err("refused");
    assert_eq!(err.to_string(), "`address` in [openbao] is not a URL");
}

#[test]
fn interval_must_be_a_nonzero_duration() {
    assert_eq!(with("interval = \"0s\""), Err(ConfigError::Interval));
    assert_eq!(with("interval = \"soon\""), Err(ConfigError::Interval));
    assert_eq!(
        with("interval = \"90s\"").map(|c| c.interval),
        Ok(Duration::from_secs(90))
    );
}

#[test]
fn environment_names_follow_shep_secret_names() {
    assert_eq!(
        with("[environments.\"prod/eu\"]\npaths = [\"a\"]"),
        Err(ConfigError::EnvironmentName("prod/eu".to_string()))
    );
    assert_eq!(
        with("[environments.\".hidden\"]\npaths = [\"a\"]"),
        Err(ConfigError::EnvironmentName(".hidden".to_string()))
    );
}

#[test]
fn an_environment_needs_paths_and_clean_segments() {
    assert_eq!(
        with("[environments.production]"),
        Err(ConfigError::NoPaths("production".to_string()))
    );
    assert_eq!(
        with("[environments.production]\npaths = [\"a/../b\"]"),
        Err(ConfigError::Path {
            environment: "production".to_string(),
            path: "a/../b".to_string(),
        })
    );
    assert_eq!(
        with("[environments.production]\npaths = [\"//\"]"),
        Err(ConfigError::Path {
            environment: "production".to_string(),
            path: "//".to_string(),
        })
    );
    assert_eq!(
        with("[environments.production]\nmount = \"a//b\"\npaths = [\"x\"]"),
        Err(ConfigError::Mount("production".to_string()))
    );
}

#[test]
fn a_path_listed_twice_is_refused_after_trimming() {
    assert_eq!(
        with("[environments.production]\npaths = [\"a/b\", \"/a/b/\"]"),
        Err(ConfigError::DuplicatePath {
            environment: "production".to_string(),
            path: "a/b".to_string(),
        })
    );
}

#[test]
fn an_unknown_key_is_refused() {
    assert!(parse_section::<Section>("openbao", "tokn = \"x\"").is_err());
    assert!(parse_section::<Section>("openbao", "[environments.p]\npath = [\"x\"]").is_err());
}

#[test]
fn debug_never_shows_the_secret_id() {
    let section = section(MINIMAL);
    assert_eq!(
        format!("{section:?}"),
        "Section { address: Some(\"https://openbao.example.com:8200\"), ca_cert: None, \
         openbao_namespace: None, role_id: Some(\"role\"), secret_id: Some(<redacted>), \
         interval: None, persist: None, environments: {} }"
    );
    let config = section.resolve().expect("resolves");
    assert_eq!(
        format!("{config:?}"),
        "Config { address: Url { scheme: \"https\", cannot_be_a_base: false, username: \"\", \
         password: None, host: Some(Domain(\"openbao.example.com\")), port: Some(8200), \
         path: \"/\", query: None, fragment: None }, ca_cert: None, openbao_namespace: None, \
         role_id: \"role\", secret_id: <redacted>, interval: 300s, persist: true, environments: {} }"
    );
}

#[test]
fn the_schema_and_the_printed_block_name_the_same_settings() {
    use shep_client::testing::{printed_keys, schema_keys};
    assert_eq!(schema_keys::<Section>(), printed_keys(PRINT_CONFIG));
}

#[test]
fn the_printed_block_parses_once_uncommented() {
    let uncommented: String = PRINT_CONFIG
        .lines()
        .filter(|line| !line.starts_with('['))
        .map(|line| line.trim_start_matches('#'))
        .collect::<Vec<_>>()
        .join("\n");
    // Parsed, not resolved: the printed `role_id` and `secret_id` are
    // empty placeholders, which resolving rightly refuses.
    let section = section(&uncommented);
    assert_eq!(
        section.environments["production"].paths,
        ["myapp/production"]
    );
    assert_eq!(section.persist, Some(true));
}

#[test]
fn the_schema_marks_the_secret_id_and_nothing_else() {
    let schema = serde_json::to_value(shep_client::dogs::config_schema::<Section>())
        .expect("the schema serialises");
    let properties = schema["properties"].as_object().expect("properties");
    let marked: Vec<&String> = properties
        .iter()
        .filter(|(_, p)| p.get(shep_client::dogs::SECRET_KEY) == Some(&serde_json::json!(true)))
        .map(|(key, _)| key)
        .collect();
    assert_eq!(marked, ["secret_id"]);
}
