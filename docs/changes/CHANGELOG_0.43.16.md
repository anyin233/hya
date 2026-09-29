# 0.43.16

## Bundles prove themselves before install and prove activation after it

- Every bundle kind may declare its own self-check in the manifest: `check: { command: [argv…], timeout_secs: 1–600 }` (default 120). `hya bundle verify` and `hya bundle install` run it in a private copy of the package sources (cwd and `HYA_BUNDLE_ROOT`, plus `HYA_BUNDLE_ID`, `HYA_BUNDLE_VERSION`); a failure, timeout, or spawn error refuses the command with the output tail and writes nothing. Without a declaration they print `self-check: none declared`.
- New rpc `Catalog.RefreshBundles` (`POST /v1/bundles:refresh`, body `{directory}`) refreshes the installed-bundle catalog and the directory's Project overlay now and returns `{generation, bundles: [{id, version, preparedDigest, scope}], errors: [{bundleId, message}], scope}`. A generation that fails to prepare keeps the previous one and is reported in `errors`; `catalog.updated` fires when a new generation is published. 18 services / 103 rpcs.
- `hya bundle install` (global `--db`, default the durable database) asks the running backend to refresh and requires the installed bundle at its prepared digest to be published: `activation: active in the backend (pid N); the next turn uses it`. Otherwise it exits 1 with `installed but not activated: …` while the backend keeps serving its previous bundles. A project install outside a registered Project says that no session loads it; without a running backend it loads when one starts.

```yaml
check:
  command: [bun, test]
  timeout_secs: 120
```
