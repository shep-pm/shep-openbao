# shep-openbao

A provider dog for shep: mirrors secrets from OpenBao into shep's secrets store. MIT OR Apache-2.0.

Pre-MVP: `src/main.rs` is still empty. It is meant to run as an external dog, adopted with `shep adopt` like shep-log-rotate.

## Commands

- `cargo test --locked` is the test shape.
- CI's gates, all required: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features --locked`, and `cargo +1.88 check --all-targets --all-features --locked` for the MSRV.
- One cargo command at a time: they share the target-dir lock.
- `rust-toolchain.toml` floats on stable, like CI. When CI's clippy flags something a local run did not, `rustup update stable` first.

## Where things live

- shep-pm/shep-log-rotate is the reference dog. Copy its shape, not its logic: `shep_client::dogs::probe` on the first line of `main`, the `dog_config` attribute on the config section, `connect_as_dog`, config under the dog's own table in `dogs.toml` (shep-log-rotate's is `[log-rotate]`), and an `integration` feature plus CI job that builds a real shep from `main`.

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
