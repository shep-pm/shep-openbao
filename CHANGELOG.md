# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-11-05

### Added

- Parse the [openbao] section
- Log in with AppRole and read KV v2
- Build an environment's set from its KV paths
- Push each environment into shep's secrets store
- Declare a verb for shep's help

### Fixed

- Refuse a KV answer with no data rather than read it as empty
- Keep pushing an environment until a push lands
- Retry an empty push for a removed environment every round
- Hear a stop while waiting on the shepherd or on a round
- Compute a token's renewal point without overflowing
- Read at most 4 MiB of any OpenBao answer
- Escape keys and paths in a mirror problem's message
- Refuse a fractional number rather than rewrite it
- Declare the secrets.write permission shep 0.22 enforces

