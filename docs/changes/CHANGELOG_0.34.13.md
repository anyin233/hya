# 0.34.13

- Add independent `hya-updater` trust boundary: ed25519-signed release metadata
  verification (trust root, sequence anti-rollback floor, platform, freshness,
  protocol/min-updater compatibility, artifact digests) with no `hya-core` /
  plugin / MCP / bundle / app / store dependencies.
- Stage immutable versioned releases under `releases/<sequence>/` from a local
  package directory (`file://` or path; no in-TCB network download), smoke
  candidates in a dedicated subprocess, and commit activation only when
  `--owner-authorized-activation` / `owner_authorized` is set.
- Ship the `hya-updater` CLI (`version`, `status`, `recover`, `apply`,
  `discard`, `init-roots`), `apply_update` pipeline, discard of uncommitted
  candidates, and crash recovery that never decrements the accepted floor.
- Document the operator path in `docs/self-update.md` and
  `docs/examples/self-update/`, and register the built-in `secure-self-update`
  skill. `install.sh` remains break-glass bootstrap/recovery.
