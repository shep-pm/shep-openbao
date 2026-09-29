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
//!     cargo test --features integration
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
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

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

fn shep_bin() -> PathBuf {
    let path = PathBuf::from(required("SHEP_BIN", "../shep/target/debug/shep"));
    assert!(
        path.is_file(),
        "$SHEP_BIN does not name a file: {}",
        path.display()
    );
    path
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

/// OpenBao as its root token sees it, for setting up what the dog reads.
struct Bao {
    address: String,
    token: String,
    http: reqwest::Client,
    runtime: tokio::runtime::Runtime,
}

/// What a test's AppRole logs in with.
struct Login {
    role_id: String,
    secret_id: String,
}

impl Bao {
    fn new() -> Self {
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
    fn approle(&self, prefix: &str) -> Login {
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
    fn put(&self, path: &str, data: Value) {
        self.ok(
            reqwest::Method::POST,
            &format!("secret/data/{path}"),
            Some(json!({ "data": data })),
        );
    }
}

/// One shepherd in its own temporary `$SHEP_HOME`, killed on drop.
struct Shepherd {
    home: tempfile::TempDir,
    shep: PathBuf,
}

impl Shepherd {
    fn new() -> Self {
        // Under /tmp rather than $TMPDIR: a unix socket path is bounded at
        // about 104 bytes, and $TMPDIR on macOS alone eats half of that.
        let home = tempfile::Builder::new()
            .prefix("sob")
            .tempdir_in("/tmp")
            .expect("a temporary $SHEP_HOME");
        Self {
            home,
            shep: shep_bin(),
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    /// Run one `shep` command against this home, from inside it so no
    /// Flockfile in the caller's directory is picked up.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.shep)
            .args(args)
            .arg("--home")
            .arg(self.home())
            .env("SHEP_HOME", self.home())
            .env_remove("SHEP_DOG_NAME")
            .current_dir(self.home())
            .output()
            .expect("shep ran")
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "shep {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn write_dogs_toml(&self, body: &str) {
        fs::write(self.home().join("dogs.toml"), body).expect("dogs.toml");
    }

    /// Start this dog as a plain child, the way an operator running it by
    /// hand would, with its output captured. With no `$SHEP_DOG_NAME` it
    /// reads `[openbao]` and pushes under `openbao`.
    fn spawn_dog(&self) -> DogProcess {
        let out = fs::File::create(self.home().join("dog.out")).expect("dog.out");
        let err = fs::File::create(self.home().join("dog.err")).expect("dog.err");
        let child = Command::new(DOG_BIN)
            .env("SHEP_HOME", self.home())
            .env_remove("SHEP_DOG_NAME")
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .expect("the dog started");
        DogProcess(child)
    }

    fn dog_stdout(&self) -> String {
        fs::read_to_string(self.home().join("dog.out")).unwrap_or_default()
    }

    fn dog_output(&self) -> String {
        let err = fs::read_to_string(self.home().join("dog.err")).unwrap_or_default();
        format!("{}{err}", self.dog_stdout())
    }

    /// Waits until the dog's stdout has said `line` at least `times` times.
    fn wait_for_dog(&self, line: &str, times: usize) {
        wait_until(
            &format!("the dog to say {line:?} {times} time(s)"),
            || self.dog_stdout().lines().filter(|l| *l == line).count() >= times,
            || self.dog_output(),
        );
    }
}

impl Drop for Shepherd {
    fn drop(&mut self) {
        let _ = self.run(&["kill", "--style", "bare"]);
    }
}

/// A dog running as a plain child, killed on drop.
struct DogProcess(Child);

impl Drop for DogProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A sheep that writes `$DB_URL` to `out`, atomically, then waits to be
/// stopped. Each start rewrites it, so a restart is visible as new content.
fn app_script(dir: &Path, out: &Path) -> PathBuf {
    let path = dir.join("app.sh");
    let mut file = fs::File::create(&path).expect("script");
    write!(
        file,
        "#!/bin/sh\nprintf '%s' \"$DB_URL\" > {out}.tmp && mv {out}.tmp {out}\nexec sleep 300\n",
        out = out.display()
    )
    .expect("script body");
    drop(file);
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path
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
