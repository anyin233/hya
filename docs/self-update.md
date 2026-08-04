# Secure self-update (0.34.13)

`hya-updater` is the independent update trust boundary. It does **not** depend
on `hya-core`, plugins, MCP, bundles, app config, or session storage.

Production activation is **owner-gated**. A valid signature is necessary but not
sufficient: the operator must pass `--owner-authorized-activation` (or set
`owner_authorized` in the library API). Network download is **outside** the TCB;
download a complete package directory first, then verify/stage/activate.

`install.sh` remains break-glass bootstrap and manual recovery.

## Layout under an updater root

```text
<root>/
  trust_roots.json      # ed25519 verifying keys (TCB)
  accepted_floor        # monotonic accepted sequence
  current               # active generation selector
  activation.journal    # prepare/commit/abort records
  releases/<sequence>/  # immutable staged artifacts
```

Control files must never live under `releases/`. Session databases and secrets
must not appear under the updater root.

## CLI

Build:

```sh
cargo build -p hya-updater --bin hya-updater
```

Commands:

```sh
# Inspect
./target/debug/hya-updater version
./target/debug/hya-updater status --root /var/lib/hya/updater

# Recover interrupted prepare/commit
./target/debug/hya-updater recover --root /var/lib/hya/updater

# Stage only (default product path without owner gate)
./target/debug/hya-updater apply \
  --root /var/lib/hya/updater \
  --metadata ./release.metadata.json \
  --package ./package-dir \
  --platform x86_64-unknown-linux-gnu \
  --smoke smoke.sh

# Owner-authorized activation (advances selector + accepted floor)
./target/debug/hya-updater apply \
  --root /var/lib/hya/updater \
  --metadata ./release.metadata.json \
  --package ./package-dir \
  --platform x86_64-unknown-linux-gnu \
  --smoke smoke.sh \
  --owner-authorized-activation

# Discard a staged-but-not-accepted candidate
./target/debug/hya-updater discard --root /var/lib/hya/updater --sequence 42
```

Bootstrap trust roots (operator only):

```sh
./target/debug/hya-updater init-roots \
  --path /var/lib/hya/updater/trust_roots.json \
  --root ci-root-1=<64-lower-hex-verifying-key>
```

## Signed metadata

Metadata is JSON. The signature covers a domain-separated canonical payload
(`hya.updater.release-metadata.v1`) that **excludes** the `signature` field.

Required fields include: `sequence`, `platform`, `artifacts[]`, `not_before`,
`not_after`, `recovery`, `protocol_version` (must be `1`),
`min_updater_version`, `key_id`, and `signature` (byte array).

Anti-rollback: `sequence` must be strictly greater than `accepted_floor`.
Recovery of older bits requires a **new higher sequence**, never a silent
downgrade.

## Example package

See [`docs/examples/self-update/`](examples/self-update/) for a local dry-run
script that signs fixture metadata, stages, and optionally activates under a
temporary root.

## Skill

Built-in skill `secure-self-update` summarizes this workflow for agents. Do not
use it to expand privileges or to skip the owner activation gate.
