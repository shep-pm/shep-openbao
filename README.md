# shep-openbao

A provider dog for [shep](https://github.com/shep-pm/shep): mirrors secrets from OpenBao into shep's secrets store.

Each environment in `[openbao]` names KV v2 paths. The dog logs in with AppRole, reads those paths every `interval`, and pushes every key it finds into its namespace of shep's secrets store. A sheep reads one in its `env`:

```sh
shep start 'DB_URL={{secret:openbao/DB_URL}}' ./server
```

The shepherd resolves the reference when it spawns the sheep. A sheep started before the dog's first push waits and retries, and one whose key is missing from a push goes `errored`, with `shep describe` naming the key.

## Install

```sh
cargo install --git https://github.com/shep-pm/shep-openbao
shep adopt shep-openbao
```

`adopt` names the dog `openbao`, which is also the namespace a sheep reads from and the section of `dogs.toml` the dog reads. Add it to `boot_first_dogs` so it starts before the flock:

```toml
# $SHEP_HOME/shep.toml
[daemon]
boot_first_dogs = ["openbao"]
```

## OpenBao

The dog only logs in and reads. A policy reaching the paths it mirrors, and an AppRole holding it:

```sh
bao auth enable approle
bao policy write shep-openbao - <<'EOF'
path "secret/data/myapp/*" { capabilities = ["read"] }
EOF
bao write auth/approle/role/shep-openbao token_policies=shep-openbao token_ttl=1h secret_id_num_uses=0
bao read -field=role_id auth/approle/role/shep-openbao/role-id
bao write -f -field=secret_id auth/approle/role/shep-openbao/secret-id
```

`secret_id_num_uses=0` matters. The dog logs in again two thirds of the way through each token's TTL, so a secret ID with a use limit stops working after that many logins.

## Configuration

`shep-openbao --print-config` prints every setting. In `$SHEP_HOME/dogs.toml`:

```toml
[openbao]
address = "https://openbao.example.com:8200"
role_id = "..."
secret_id = "..."

[openbao.environments.production]
paths = ["myapp/production", "shared/production"]
```

| key | default | |
|---|---|---|
| `address` | required | `https` anywhere, or `http` to a loopback address only |
| `ca_cert` | none | a PEM file to trust beside the system's own roots |
| `openbao_namespace` | none | the OpenBao namespace to work in, for every environment |
| `role_id` | required | the AppRole role ID |
| `secret_id` | required | the AppRole secret ID, masked in lookout |
| `interval` | `5m` | how often every environment is read |
| `persist` | `true` | read by the shepherd: whether pushed values are cached on disk |
| `environments.<name>.mount` | `secret` | the KV v2 mount |
| `environments.<name>.paths` | required | the KV paths whose keys are mirrored |

An environment's name is the one a sheep resolves against, `production` unless the sheep or the shepherd sets another. Every key at an environment's paths becomes a secret, and the AppRole's policy is what decides which paths the dog can read.

Config changes apply without restarting the dog. A change that does not parse is logged, and the running settings stay.

## Worth knowing

- A value changed in OpenBao reaches a running sheep at its next restart. The dog pushes it within `interval`, but a running process's environment cannot change.
- Nothing a round finds wrong changes what the shepherd holds. A path that fails to read, a key found at two paths, or a key shep would refuse as a name keeps the last push for that environment, and the log names the path or key, never a value.
- Pushed values are cached in `$SHEP_HOME/secrets-cache.json` (mode `0600`), so a reboot does not wait on OpenBao. `persist = false` keeps them in memory only.
- An environment removed from `[openbao]` while the dog runs gets an empty push. Removed while the dog is down, its values stay in `secrets-cache.json` until that file is deleted.
- Only KV v2, and only strings, numbers and booleans. A `null`, list or table value refuses its environment's push.

## License

MIT OR Apache-2.0, at your option.
