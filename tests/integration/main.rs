//! The tier that drives a REAL shepherd and a REAL OpenBao.
//!
//! The unit tests fake both ends, and every one of them can pass while this
//! dog pushes nothing a sheep can read: a namespace the shepherd files under
//! another name, a push the shepherd refuses, a template that resolves
//! against the wrong environment. Only a sheep reading the value back out of
//! its own environment proves the path works, and that is what each test
//! here ends on.
//!
//! Gated behind the `integration` feature, and needing a built `shep` at
//! `$SHEP_BIN` and an OpenBao dev server at `$BAO_ADDR` whose root token is
//! `$BAO_TOKEN` (default `root`):
//!
//! ```text
//! bao server -dev -dev-root-token-id=root -dev-listen-address=127.0.0.1:18200 &
//! SHEP_BIN=../shep/target/debug/shep BAO_ADDR=http://127.0.0.1:18200 \
//!     cargo test --features integration --locked --test integration
//! ```
//!
//! # $SHEP_HOME is a temporary directory in every test, and that is load-bearing
//!
//! A live shepherd runs at `~/.shep` supervising real services. Every test
//! builds its own [`Shepherd`], which owns a temporary directory and kills the
//! daemon it booted when it drops. `SHEP_HOME` goes into every child's
//! environment as well as `--home`, because `shep adopt` vets a dog by
//! spawning it with this process's environment, and a dog with no
//! `SHEP_HOME` would connect to the operator's real shepherd and push there.
//!
//! Each test also works under its own KV prefix and AppRole, so the tests can
//! share one OpenBao and run in parallel.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::json;

mod bao;
mod shepherd;

use bao::{Bao, Login};
use shepherd::{DogProcess, Shepherd, app_script};

/// This crate's own binary, as cargo built it for this run.
const DOG_BIN: &str = env!("CARGO_BIN_EXE_shep-openbao");

/// How long any wait gives up after. Generous: these tests boot a daemon and
/// talk to a server, and a contended machine is slow rather than broken.
const PATIENCE: Duration = Duration::from_secs(60);

/// A required environment variable, loudly. A tier that quietly skipped
/// itself would be the failure this file exists to avoid.
fn required(key: &str, example: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| {
        panic!("the integration tier needs ${key}, for example {key}={example}")
    })
}

/// A name no other test in this run, or in an earlier run against the same
/// OpenBao, will pick.
fn unique(label: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{label}-{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Poll `ready` until it answers true, or fail with `what` and whatever
/// `context` says about the state of things.
fn wait_until(what: &str, ready: impl Fn() -> bool, context: impl Fn() -> String) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "timed out waiting for {what}\n--- context ---\n{}",
        context()
    );
}

/// Everything one test sets up: an AppRole, a KV prefix holding `DB_URL`,
/// a shepherd, and the dog running against both.
struct World {
    bao: Bao,
    prefix: String,
    login: Login,
    shepherd: Shepherd,
}

impl World {
    fn new(db_url: &str) -> Self {
        let bao = Bao::new();
        let prefix = unique("sob");
        let login = bao.approle(&prefix);
        bao.put(&format!("{prefix}/app"), json!({ "DB_URL": db_url }));
        Self {
            bao,
            prefix,
            login,
            shepherd: Shepherd::new(),
        }
    }

    /// `[section]` pointing at this world's OpenBao, one second between
    /// rounds, with `extra` appended to the section.
    fn section(&self, section: &str, extra: &str) -> String {
        format!(
            "[{section}]\n\
             address = \"{address}\"\n\
             role_id = \"{role_id}\"\n\
             secret_id = \"{secret_id}\"\n\
             interval = \"1s\"\n\
             {extra}\n\
             [{section}.environments.production]\n\
             paths = [\"{prefix}/app\"]\n",
            address = self.bao.address,
            role_id = self.login.role_id,
            secret_id = self.login.secret_id,
            prefix = self.prefix,
        )
    }

    fn out(&self) -> PathBuf {
        self.shepherd.home().join("app.out")
    }

    fn read_out(&self) -> String {
        fs::read_to_string(self.out()).unwrap_or_default()
    }

    /// Starts the sheep reading `DB_URL` from `namespace`.
    fn start_app(&self, namespace: &str) {
        let script = app_script(self.shepherd.home(), &self.out());
        let assignment = format!("DB_URL={{{{secret:{namespace}/DB_URL}}}}");
        self.shepherd.ok(&[
            "start",
            &assignment,
            script.to_str().expect("script path"),
            "--name",
            "app",
            "--style",
            "bare",
        ]);
    }

    fn wait_for_app(&self, value: &str) {
        wait_until(
            &format!("the sheep to read {value:?}"),
            || self.read_out() == value,
            || {
                format!(
                    "app.out: {:?}\n{}\n{}",
                    self.read_out(),
                    self.shepherd.dog_output(),
                    String::from_utf8_lossy(&self.shepherd.run(&["flock"]).stdout)
                )
            },
        );
    }
}

/// Boots the shepherd with nothing running, the dog beside it, and waits
/// for the first push.
fn boot(world: &World, extra: &str) -> DogProcess {
    world
        .shepherd
        .write_dogs_toml(&world.section("openbao", extra));
    world.shepherd.ok(&["start", "--style", "bare"]);
    let dog = world.shepherd.spawn_dog();
    world
        .shepherd
        .wait_for_dog("production: pushed 1 secret: DB_URL", 1);
    dog
}

#[test]
fn a_sheep_reads_the_value_the_dog_pushed() {
    let world = World::new("postgres://from-openbao");
    let _dog = boot(&world, "");

    world.start_app("openbao");
    world.wait_for_app("postgres://from-openbao");

    // The credential and the value went through the dog, and neither may
    // come out of it.
    let said = world.shepherd.dog_output();
    assert!(!said.contains(&world.login.secret_id), "{said}");
    assert!(!said.contains("postgres://from-openbao"), "{said}");
}

#[test]
fn a_changed_value_reaches_the_sheep_at_its_next_restart() {
    let world = World::new("postgres://first");
    let _dog = boot(&world, "");
    world.start_app("openbao");
    world.wait_for_app("postgres://first");

    world.bao.put(
        &format!("{}/app", world.prefix),
        json!({"DB_URL": "postgres://second"}),
    );
    world
        .shepherd
        .wait_for_dog("production: pushed 1 secret: DB_URL", 2);
    // Still the old value: a running process's environment cannot change.
    assert_eq!(world.read_out(), "postgres://first");

    world.shepherd.ok(&["restart", "app", "--style", "bare"]);
    world.wait_for_app("postgres://second");
}

#[test]
fn a_key_deleted_in_openbao_errors_the_sheep_at_its_next_restart() {
    let world = World::new("postgres://doomed");
    let _dog = boot(&world, "");
    world.start_app("openbao");
    world.wait_for_app("postgres://doomed");

    world
        .bao
        .put(&format!("{}/app", world.prefix), json!({"OTHER": "x"}));
    world
        .shepherd
        .wait_for_dog("production: pushed 1 secret: OTHER", 1);
    let _ = fs::remove_file(world.out());

    // shep refuses at once rather than retrying, since the namespace has
    // been pushed and simply lacks the key: `spawn_failed`, exit 7.
    let restart = world.shepherd.run(&["restart", "app", "--style", "bare"]);
    assert_eq!(restart.status.code(), Some(7), "{restart:?}");
    let described = world.shepherd.ok(&["describe", "app"]);
    assert!(described.contains(" errored "), "{described}");
    assert!(
        described.contains("openbao/DB_URL (production): missing"),
        "shep names the missing key: {described}"
    );
    assert!(
        !world.out().exists(),
        "the sheep never ran without its secret"
    );
}

#[test]
fn a_reload_with_persist_off_is_pushed_again() {
    let world = World::new("postgres://kept");
    let _dog = boot(&world, "persist = false");
    world.start_app("openbao");
    world.wait_for_app("postgres://kept");

    world.shepherd.ok(&["daemon", "reload", "--style", "bare"]);
    world.shepherd.wait_for_dog(
        "reconnected to the shepherd, pushing every environment again",
        1,
    );
    world
        .shepherd
        .wait_for_dog("production: pushed 1 secret: DB_URL", 2);

    let _ = fs::remove_file(world.out());
    world.shepherd.ok(&["restart", "app", "--style", "bare"]);
    world.wait_for_app("postgres://kept");
}

#[test]
fn an_adopted_dog_pushes_under_the_name_it_was_adopted_as() {
    let world = World::new("postgres://adopted");
    world.shepherd.write_dogs_toml(&world.section("vault", ""));
    world.shepherd.ok(&["start", "--style", "bare"]);
    world
        .shepherd
        .ok(&["adopt", DOG_BIN, "--name", "vault", "--style", "bare"]);

    // Waiting on the sheep rather than on the dog's own output, which an
    // adopted dog writes to the shepherd's logs: the sheep reading the value
    // is the proof that matters. It may start before the first push, and
    // shep retries it until the namespace is populated.
    world.start_app("vault");
    world.wait_for_app("postgres://adopted");
}

#[test]
fn a_stop_while_the_shepherd_is_gone_is_a_clean_exit() {
    let world = World::new("postgres://any");
    let mut dog = boot(&world, "");

    // The dog's event stream ends and it starts waiting, up to five
    // seconds, for a shepherd to come back. A second in, it is inside that
    // wait when the stop arrives.
    world.shepherd.ok(&["kill", "--style", "bare"]);
    std::thread::sleep(Duration::from_secs(1));
    dog.terminate();

    let status = dog.exit_within(Duration::from_secs(3));
    assert!(
        status.success(),
        "a stop is not a lost shepherd: {status:?}\n{}",
        world.shepherd.dog_output()
    );
}
