//! A provider dog for shep: mirrors secrets from OpenBao into shep's secrets store.

mod bao;
mod config;
#[cfg(test)]
mod fake_bao;
mod secret;

fn main() {
    shep_client::dogs::probe::<config::Section>(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
}
