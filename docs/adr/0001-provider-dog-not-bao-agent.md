# A provider dog, not a `bao agent` wrapper

OpenBao Agent's process supervisor mode already renders secrets into a child's environment, so every sheep could run `bao agent` wrapped around its real command. We push secrets into shep's secrets store as a provider dog instead. A wrapper makes shep supervise the agent rather than the app: the PID, signals, exit codes and restart policy all belong to the wrapper. It also gives every sheep its own OpenBao login, and it moves the list of secrets a sheep reads out of that sheep's `env` and into an agent file. shep's secrets-store design already defines the provider-dog contract this uses (decision 6, "Push, not pull").

## Considered options

- **`bao agent` per sheep.** Rejected for the reasons above. It is also in public beta in OpenBao.
- **Writing values into a sheep's `env` with `SetSheepEnvBatch`.** Rejected: those land in `overrides.json`, the operator-override store, which is authored intent rather than a cache, and shows every value as an operator edit.
- **The shepherd asking the dog for a value at spawn.** Not available: shep's spec rules it out, because `assemble` is synchronous on the hottest path and nothing lets the shepherd send a request to a dog.

## Consequences

- A sheep reads a secret only when it spawns. A value that changes in OpenBao reaches a running sheep at its next restart, and restarting on change is follow-up work, opted into per sheep.
- Pushed values are cached in `$SHEP_HOME/secrets-cache.json` by default. `persist = false` in `[openbao]` turns that off.
