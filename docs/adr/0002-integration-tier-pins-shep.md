# Pull requests build a pinned shep; only `main` builds shep's `main` tip

The `integration` job built shep from the tip of shep's `main` on every run, pull requests included, on the grounds that an external dog's integration tier should notice shep's surface moving under it. A pull request builds a pinned shep release now: `SHEP_PIN` in `.github/workflows/test.yml`, set to the tag of the `shep` package released beside the `shep-client` version `Cargo.toml` pins. The main-tip build stays in CI, on pushes to `main` and on a weekly schedule, so the watch survives and stops gating pull requests.

## Considered options

- **Build shep's `main` tip on every pull request.** What this repo did, and what closed #8. That bump changed action refs in `.github/workflows/`; the `integration` job's only action step is the same `actions/checkout` the four passing jobs use, and every step after it is a `run:` step that fetches shep's tip and OpenBao 2.7.0. Nothing in the diff could reach them, so the run went red on something the pull request had no part in. #12 broke the same way. A gate that can go red no matter what a pull request changes is not a gate: it teaches people to close good work and re-run it.
- **Make `integration` not required.** Rejected: it is the only job that runs a real shepherd against a real OpenBao, and the only one that proves a sheep can read a mirrored value back out of its own environment. A red run has to block.
- **Pin a commit SHA rather than a release tag.** Kept as the escape hatch, not the default. A SHA is what you record when a fix you need sits between two releases, and it says nothing about which release the pair is.
- **Read the pin out of `Cargo.toml` at run time.** Rejected: it saves one line and adds a parsing step that can itself fail, and a literal in the workflow is greppable.

## Consequences

- The pinned pair is the pair that ships. The dog compiles against `shep-client` 0.24.2 and now runs against the shepherd released beside it, which is what the tier is really checking: wire compatibility between the two. Against shep's tip it tested a pair that never ships together.
- The tier's other input was already deterministic. OpenBao is pinned by version, and its tarball is checked against the release's own checksums before anything runs. shep was the one unpinned input.
- `SHEP_PIN` and the `shep-client` dependency have to move together, and nothing checks that they do. Whoever bumps the dependency bumps the pin.
- shep's `main` tip is watched on a slower clock: a break shows up at the next push to `main`, or at the weekly run, at most a week later. A red weekly run belongs to no pull request. Whoever reads it decides whether to move the pin or file against shep.
- The weekly schedule triggers the whole `test` workflow, so every gate re-runs weekly. That re-checks the four toolchain gates against whatever `stable` has become in the meantime.
- On a push to `main` the same job builds shep's tip, so a broken tip can hold a direct push to a protected `main`. A merge is gated by the pull request's own run, which is pinned.
