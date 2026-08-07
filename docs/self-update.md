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

# Optional: verify against trust roots outside <root>/trust_roots.json
# (e.g. read-only media or a staged key set during rotation)
./target/debug/hya-updater apply \
  --root /var/lib/hya/updater \
  --metadata ./release.metadata.json \
  --package ./package-dir \
  --platform x86_64-unknown-linux-gnu \
  --trust-roots /secure/media/trust_roots.json \
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

### `trust_roots.json` format

On disk the file is JSON:

```json
{
  "roots": [
    {
      "key_id": "ci-root-1",
      "verifying_key_hex": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    }
  ]
}
```

Constraints ([`trust.rs`](../crates/hya-updater/src/trust.rs)):

| Rule | Detail |
| --- | --- |
| At least one root | An empty `roots` array is rejected. |
| `key_id` | Must be non-empty. |
| `verifying_key_hex` | Exactly **64 lower-hex** characters (32-byte Ed25519 verifying key). **Uppercase hex is rejected** — a common hand-editing trap. |

## Signed metadata

Metadata is JSON. The signature covers a domain-separated canonical payload
(`hya.updater.release-metadata.v1`) that **excludes** the `signature` field.

Required fields include: `sequence`, `platform`, `artifacts[]`, `not_before`,
`not_after`, `recovery`, `protocol_version` (must be `1`),
`min_updater_version`, `key_id`, and `signature` (byte array).

### Metadata field validation

Before signing or verifying the canonical payload
([`canonical_metadata_payload`](../crates/hya-updater/src/verify.rs)):

| Field | Constraint |
| --- | --- |
| `platform` | Non-empty. |
| `key_id` | Non-empty. |
| `min_updater_version` | Non-empty. Used as a gate (see below), not only as documentation. |
| `not_before` / `not_after` | Unix seconds; `not_after` must be **≥** `not_before` (inclusive window). |
| `artifacts` | Non-empty list. |
| Each artifact `name` | Non-empty. |
| Each artifact `sha256_hex` | Exactly **64 lower-hex** characters. Uppercase hex is rejected. |

### `apply` flags

| Flag | Role |
| --- | --- |
| `--root` | Updater root directory (control files + `releases/`). |
| `--metadata` | Path to signed release metadata JSON. |
| `--package` | Local package directory (or `file://` URL) with named artifacts. |
| `--platform` | Host platform triple; must match `metadata.platform`. |
| `--smoke` | Optional relative smoke command under the staged release. |
| `--owner-authorized-activation` | Required to advance the selector and accepted floor. |
| `--trust-roots <PATH>` | Override path to `trust_roots.json` (default: `<root>/trust_roots.json`). Use when keys live on separate/read-only media or when verifying against a staged key set during rotation. |

### Verification gate chain (`apply`)

`verify_release_metadata` runs gates **in this order**. The first failure is
what the operator sees:

1. **`protocol_version`** — must equal the supported value (`1`).
2. **`min_updater_version`** — compared to this crate’s version with
   dotted-numeric compare (`1.2.3`); if the running updater is **older** than
   the metadata requirement → `UpdaterTooOld`.
3. **`sequence`** — must be **strictly greater** than `accepted_floor`
   (anti-rollback). Recovery of older bits requires a **new higher sequence**,
   never a silent downgrade.
4. **`platform`** — must equal the host platform string passed to verify.
5. **Time window** — `now_unix` must be ≥ `not_before` and ≤ `not_after`.
6. **Trust root + signature** — look up `key_id` in `trust_roots.json`, then
   verify the Ed25519 signature over the domain-separated canonical payload.

## Staging and smoke

### Staging (`stage_verified_release`)

Staging writes only under `root/releases/<sequence>/` and **never mutates** an
existing staged generation ([`stage.rs`](../crates/hya-updater/src/stage.rs)):

- Creates `releases/<sequence>` and **errors if that directory already exists**
  (re-applying the same sequence fails rather than overwriting).
- For each artifact: rejects absolute names and path segments containing `..`;
  re-verifies **size** and **SHA-256** against the verified metadata before
  writing; `fsync`s each file; on Unix sets mode **`0o755`**.
- After writes, confirms every declared artifact is present as a file under the
  stage directory.

### Smoke (`--smoke`)

The smoke command path must be **relative** and must not contain `..`
([`smoke.rs`](../crates/hya-updater/src/smoke.rs)). It is executed as a **child
process** with cwd set to the staged release directory — never loaded into the
updater’s address space. A non-zero exit is reported as **`SmokeFailed`**.

```sh
# relative_command is resolved inside releases/<sequence>/
--smoke smoke.sh
```

## Activation recovery

`recover` reads `activation.journal` and the selector and applies exactly one of
three outcomes ([`recover_activation`](../crates/hya-updater/src/journal.rs)):

| Case | Behavior |
| --- | --- |
| No journal, or last phase is **`committed`** or **`aborted`** | Keep the current selector unchanged. |
| Last phase is **`prepare`**, and the selector still points at the **previous** generation | Write an **`aborted`** journal record and keep the old generation (crash before selector rename). |
| Last phase is **`prepare`**, and the selector **already** points at the candidate | Finish activation: raise the accepted floor if needed and write a **`committed`** record. |

Recovery never leaves a mixed selector/floor and never decrements the accepted
floor.

## Discard staged candidates

`discard --sequence N` removes a staged-but-not-accepted directory only when it
is safe ([`discard_staged_release`](../crates/hya-updater/src/pipeline.rs)). It
**refuses** when:

1. **`sequence` is `0`**
2. **`sequence` is the currently selected generation**
3. **`sequence` is at or below the accepted floor**
4. **The staged directory is absent**

Safety property: discard can only ever remove bits that were **never accepted**.

## Example package

See [`docs/examples/self-update/`](examples/self-update/) for a local dry-run
script that signs fixture metadata, stages, and optionally activates under a
temporary root.

## Skill

Built-in skill `secure-self-update` summarizes this workflow for agents. Do not
use it to expand privileges or to skip the owner activation gate.
