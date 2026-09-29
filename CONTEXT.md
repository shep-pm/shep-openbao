# shep-openbao

A provider dog for shep that mirrors secrets out of OpenBao into shep's secrets store, where sheep read them as environment variables. shep's own vocabulary (sheep, flock, shepherd, dog) holds here unchanged, as defined in shep's `docs/terminology.md`.

## Language

### shep's side

**Provider dog**:
A dog that pushes secrets into shep's secrets store under its own namespace. shep-openbao is one.
_Avoid_: secrets dog, injector, agent, sidecar

**Namespace**:
The slice of shep's secrets store that one provider dog owns, named after the dog. This dog's is `openbao` by default, so a sheep reads `{{secret:openbao/NAME}}`. Never OpenBao's own namespaces, which are always written out in full.
_Avoid_: prefix, scope

**Secret**:
One named value in a namespace, which a sheep reads into one environment variable. AppRole's secret ID is not one: it is the dog's own credential for logging in to OpenBao.
_Avoid_: credential, key

**Environment**:
shep's name for the deployment a sheep resolves its secrets against, such as `production`. Every push is for exactly one environment.
_Avoid_: stage, target, profile

**Push**:
One delivery of a namespace's complete set of secrets for one environment, replacing whatever it held for that environment before.
_Avoid_: sync, upload, update

### OpenBao's side

**OpenBao namespace**:
An isolated tenant inside one OpenBao server, with its own mounts and policies.
_Avoid_: bare "namespace", which always means shep's

**KV path**:
A location in an OpenBao KV v2 mount, holding a map of keys to values. Each key at a mirrored KV path becomes one secret.
_Avoid_: calling the path itself a secret, as OpenBao's own docs do

**Mirror**:
To push every key found at the configured KV paths, rather than a hand-picked list of keys.
_Avoid_: sync, import, copy
