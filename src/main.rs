//! A provider dog for shep: mirrors secrets from OpenBao into shep's secrets store.
//!
//! Each environment in `[openbao]` names KV v2 paths. Every round, the dog
//! logs in with AppRole, reads those paths, and pushes every key it found
//! into its namespace of shep's secrets store with `Request::PutSecrets`,
//! where a sheep reads one with `{{secret:openbao/NAME}}`. `CONTEXT.md` has
//! the vocabulary and `docs/adr/0001-provider-dog-not-bao-agent.md` the
//! reason this is a dog rather than `bao agent` around each sheep.

mod bao;
mod config;
mod dog;
#[cfg(test)]
mod fake_bao;
mod mirror;
mod run;
mod secret;
mod shepherd;

use std::process::ExitCode;

use shep_client::{
    dogs::{self, DogAction, DogIdentity, DogRuntime, Stop},
    shep_core::exit,
};

/// The dog's name when no shepherd named it: `shep adopt shep-openbao`
/// strips the `shep-` prefix, and the name is also the namespace it pushes
/// under and the `dogs.toml` section it reads.
const DEFAULT_NAME: &str = "openbao";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    dogs::probe::<config::Section>(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    let args: Vec<String> = std::env::args().skip(1).collect();
    match dogs::parse_args(env!("CARGO_PKG_NAME"), args.iter().map(String::as_str)) {
        Ok(DogAction::Run) => {}
        Ok(DogAction::PrintConfig) => {
            print!("{}", config::PRINT_CONFIG);
            return ExitCode::SUCCESS;
        }
        Err(usage) => {
            eprintln!("{usage}");
            return usage.exit_code().into();
        }
    }

    let stop = Stop::on_stop_signals();
    let paths = match dogs::resolve_paths(&|key| std::env::var_os(key)) {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("shep-openbao: {err}");
            return err.exit_code().into();
        }
    };
    let identity = DogIdentity::from_env(&|key| std::env::var(key).ok(), DEFAULT_NAME);
    let runtime = match DogRuntime::start(identity, paths).await {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("shep-openbao: {err}");
            return err.exit_code().into();
        }
    };
    let section: config::Section = match runtime.config() {
        Ok(section) => section,
        Err(err) => {
            eprintln!("shep-openbao: {err}");
            return err.exit_code().into();
        }
    };
    let config = match section.resolve() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("shep-openbao: {err}");
            return exit::INVALID_CONFIG.into();
        }
    };
    let name = runtime.identity().section().to_string();
    dog::run(runtime.client(), name, config, stop).await
}
