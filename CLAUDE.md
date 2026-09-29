# shep-openbao

A provider dog for shep: mirrors secrets from OpenBao into shep's secrets store. MIT OR Apache-2.0.

An external dog: an operator adopts it with `shep adopt shep-openbao`, and it pushes into shep's secrets store with `Request::PutSecrets`. `CONTEXT.md` is the vocabulary and `docs/adr/` the decisions. One binary, no library target.

## Commands

- `cargo test --locked` is the test shape: the unit tier plus `tests/probe.rs`, which spawns the binary the way `shep adopt` does.
- The integration tier needs a real shepherd and a real OpenBao: `bao server -dev -dev-root-token-id=root -dev-listen-address=127.0.0.1:18200 &`, then `SHEP_BIN=<built shep> BAO_ADDR=http://127.0.0.1:18200 cargo test --features integration --locked --test integration`, the same command CI runs. Port 18200 keeps clear of a real OpenBao on 8200. Each test works under its own KV prefix and AppRole, so one dev server serves the whole tier.
- CI's gates, all required: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features --locked`, and `cargo +1.88 check --all-targets --all-features --locked` for the MSRV.
- One cargo command at a time: they share the target-dir lock.
- `rust-toolchain.toml` floats on stable, like CI. When CI's clippy flags something a local run did not, `rustup update stable` first.

## Where things live

- `secret::Secret` is the only type a credential or a mirrored value lives in: no `Display`, and a `Debug` that never reads the value. Anything new that holds one uses it, including config fields, which deserialize straight into it.
- `bao.rs` makes the only two OpenBao calls. No error there carries a response body, and `BaoError::Decode` drops serde's message on purpose: serde quotes the value it failed on, which a test in `bao/value.rs` pins.
- `mirror::build` refuses a whole environment over any one problem, because a push replaces everything that environment held. A partial set would delete secrets a sheep depends on.
- `run::Mirror` is the state between rounds and `dog.rs` the loop around it. `shepherd::Shepherd` is a trait only so the loop can be tested: shep-client's `FakeDaemon` answers `PutSecrets` with `Pong` and records nothing.
- The event stream ending is how the loop learns of a reconnect, and it forces a push of every environment: a new shepherd with `persist = false` holds nothing this dog pushed before.
- The namespace is the dog's registered name (`DogIdentity::section`), `openbao` unless adopted with `--name`. shep reads `persist` from the `dogs.toml` section of the same name.
- `SHEPHERD_RETURN_BUDGET` in `dog.rs` copies shep-daemon's `DOG_SILENCE_BUDGET`, five seconds, which is not exported to a dog outside shep's workspace. Move it if shep's moves.

## Style

- Invoke the `rust-house-style` skill before writing or reviewing Rust. The rules are shep-pm/rust-house-style, IR-1..IR-48. This repo's exceptions go in `docs/rust-house-style-addendum.md`, which wins where the two disagree.
- `#![forbid(unsafe_code)]` holds through `[lints]` in Cargo.toml.
- shep's vocabulary: a `sheep` is one managed process, the plural is `flock`, dogs are plugin processes, and the daemon is only ever "the shepherd".
- Committed text says "the maintainer", never a name, and uses repo-relative paths.

## Commits and pull requests

- Conventional subjects: `type(scope): summary`, with types `feat` `fix` `perf` `refactor` `docs` `test` `ci` `chore` `style`, and `!` on the commit that breaks something. `.githooks/commit-msg` checks locally (run `git config core.hooksPath .githooks` once per clone) and `.github/workflows/commits.yml` checks every pull request.
- Pull request titles are conventional too. `merge_commit_title` is set to the PR title, so a merge commit's subject is the title.
- Merge with a merge commit, never a squash.
- One commit per item. Bodies carry the full reasoning.

## Agent skills

### Issue tracker

Issues live in this repo's GitHub Issues, managed with `gh`. See `docs/agents/issue-tracker.md` before you create, read, label or close an issue.

### Triage labels

The five triage roles use their default label names. See `docs/agents/triage-labels.md` before you apply a triage label.

### Domain docs

Single-context: one `CONTEXT.md` plus `docs/adr/` at the repo root. See `docs/agents/domain.md` before you explore the codebase or name a domain concept.
