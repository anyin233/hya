# 0.43.17

## Self-update: owner authorization CLI, first-party rollback, attributed bundle errors

- New `hya update authorize --root DIR --sequence N --out FILE [--yes]`: the owner binds release `N` to the active generation and writes the capability `hya update apply --authorization FILE` activates. It asks for confirmation at a terminal and refuses without one unless `--yes`. The self-update demo (`docs/examples/self-update/run-demo.sh`) activates through it again (it still used the removed `--owner-authorized-activation`).
- Restart rollback in a source checkout now also restores the first-party bundles: the running build copies the in-tree first-party sources it loaded to `<db>.server.gen/<pid>/first-party/`, and the rollback successor loads them through the new `HYA_FIRST_PARTY_SOURCE_ROOT` (ordinary successors never inherit it).
- `Catalog.RefreshBundles` names the bundle behind a runtime failure (`errors[].bundleId`), and `hya bundle install` prints it: `installed but not activated; … \n  acme/dead-process: start bundle process: …`. Failed turns keep their existing error codes.

```sh
hya update authorize --root /var/lib/hya/updater --sequence 42 --out ./activation.authorization.json
hya update apply --root /var/lib/hya/updater --metadata release.metadata.json \
  --package ./package-dir --platform x86_64-unknown-linux-gnu --authorization ./activation.authorization.json
```
