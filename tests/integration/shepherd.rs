//! A shepherd in a temporary home, the dog beside it, and the sheep the tests
//! start.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
};

use super::{DOG_BIN, required, wait_until};

fn shep_bin() -> PathBuf {
    let path = PathBuf::from(required("SHEP_BIN", "../shep/target/debug/shep"));
    assert!(
        path.is_file(),
        "$SHEP_BIN does not name a file: {}",
        path.display()
    );
    path
}

/// One shepherd in its own temporary `$SHEP_HOME`, killed on drop.
pub struct Shepherd {
    home: tempfile::TempDir,
    shep: PathBuf,
}

impl Shepherd {
    pub fn new() -> Self {
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

    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// Run one `shep` command against this home, from inside it so no
    /// Flockfile in the caller's directory is picked up.
    pub fn run(&self, args: &[&str]) -> Output {
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

    pub fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "shep {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    pub fn write_dogs_toml(&self, body: &str) {
        fs::write(self.home().join("dogs.toml"), body).expect("dogs.toml");
    }

    /// Start this dog as a plain child, the way an operator running it by
    /// hand would, with its output captured. With no `$SHEP_DOG_NAME` it
    /// reads `[openbao]` and pushes under `openbao`.
    pub fn spawn_dog(&self) -> DogProcess {
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

    pub fn dog_stdout(&self) -> String {
        fs::read_to_string(self.home().join("dog.out")).unwrap_or_default()
    }

    pub fn dog_output(&self) -> String {
        let err = fs::read_to_string(self.home().join("dog.err")).unwrap_or_default();
        format!("{}{err}", self.dog_stdout())
    }

    /// Waits until the dog's stdout has said `line` at least `times` times.
    pub fn wait_for_dog(&self, line: &str, times: usize) {
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
pub struct DogProcess(Child);

impl DogProcess {
    /// Sends SIGTERM, what a shepherd stopping its dogs sends first.
    pub fn terminate(&self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .expect("kill ran");
        assert!(status.success(), "kill -TERM failed");
    }

    /// Waits up to `budget` for the dog to exit on its own, and answers how
    /// it exited. Polled, so a dog that never stops fails with a sentence
    /// rather than hanging the tier.
    pub fn exit_within(&mut self, budget: std::time::Duration) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + budget;
        while std::time::Instant::now() < deadline {
            if let Some(status) = self.0.try_wait().expect("the dog's exit status") {
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the dog is still running {budget:?} after it was asked to stop");
    }
}

impl Drop for DogProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A sheep that writes `$DB_URL` to `out`, atomically, then waits to be
/// stopped. Each start rewrites it, so a restart is visible as new content.
pub fn app_script(dir: &Path, out: &Path) -> PathBuf {
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
