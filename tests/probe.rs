//! The two questions `shep adopt` asks this binary before it records
//! anything, asked the way shep asks them.
//!
//! Spawned rather than calling `probe` directly, because what can regress is
//! the order: `probe` has to run before the argument parser, which refuses
//! every flag it does not know. A test calling `probe` would pass on a
//! `main` that never called it.
//!
//! Not gated: nothing here needs a shepherd or an OpenBao, only this crate's
//! own binary, which `cargo test` has already built.

use std::process::{Command, Output};

use shep_client::shep_core::{
    dogs::parse_version_answer,
    protocol::{MIN_SUPPORTED, PROTOCOL_VERSION},
};

/// The binary under test, as cargo built it for this run.
const DOG_BIN: &str = env!("CARGO_BIN_EXE_shep-openbao");

/// Runs the dog with one argument and no shep environment, the way `shep
/// adopt` asks a candidate it has not registered yet.
fn probe(flag: &str) -> Output {
    Command::new(DOG_BIN)
        .arg(flag)
        .env_remove("SHEP_HOME")
        .env_remove("SHEP_DOG_NAME")
        .output()
        .expect("the dog binary ran")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

/// Status 0 and nothing on stderr: a non-zero status is how `adopt` reads
/// "no answer", and shep never reads stderr, so output there is invisible.
fn answered_cleanly(output: &Output, flag: &str) {
    assert!(
        output.status.success(),
        "{flag} exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{flag} wrote to stderr, which shep never reads: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_version_answer_is_one_the_shepherds_own_parser_reads() {
    let output = probe("--version");
    answered_cleanly(&output, "--version");
    let parsed = parse_version_answer(&stdout(&output))
        .expect("an answer the shepherd's own parser cannot read is the bug this pins");
    assert_eq!(parsed.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(parsed.protocol, Some(PROTOCOL_VERSION));
}

#[test]
fn the_protocol_this_dog_announces_is_one_a_shepherd_still_accepts() {
    let parsed = parse_version_answer(&stdout(&probe("--version"))).expect("a readable answer");
    let announced = parsed.protocol.expect("a protocol line");
    // A literal, because both constants come from the one shep-core this
    // crate links and would move together. Failing here is the prompt to
    // run the integration tier against a shepherd from the new release
    // before moving the number.
    assert_eq!(
        announced, 10,
        "this dog announces protocol {announced}, and was last verified against a shepherd \
         speaking 10. Run the integration tier against the new shep before moving this."
    );
    assert!(announced >= MIN_SUPPORTED);
}

#[test]
fn the_schema_answer_is_json_naming_this_dogs_settings_and_marking_the_secret() {
    let output = probe("--schema");
    answered_cleanly(&output, "--schema");
    let schema: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("the answer is JSON");
    let properties = schema["properties"]
        .as_object()
        .expect("a form is built out of properties");
    let mut named: Vec<&str> = properties.keys().map(String::as_str).collect();
    named.sort_unstable();
    assert_eq!(
        named,
        [
            "address",
            "ca_cert",
            "environments",
            "interval",
            "openbao_namespace",
            "persist",
            "role_id",
            "secret_id",
        ]
    );
    assert_eq!(
        properties["secret_id"][shep_client::dogs::SECRET_KEY],
        serde_json::json!(true),
        "lookout masks only what the schema marks"
    );
}

#[test]
fn the_dogs_own_flag_still_reaches_the_dogs_own_parser() {
    let output = probe("--print-config");
    answered_cleanly(&output, "--print-config");
    assert!(
        stdout(&output).starts_with("[openbao]\n"),
        "--print-config printed: {}",
        stdout(&output)
    );
}

#[test]
fn a_flag_this_binary_has_never_had_is_still_refused() {
    let output = probe("--push-now");
    assert!(!output.status.success(), "an unknown flag is refused");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--push-now"),
        "the refusal names the flag: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
