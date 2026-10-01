# Secure self-update (0.43.4)

The `hya-updater` crate is the independent update trust boundary. It does
**not** depend on `hya-core`, plugins, MCP, bundles, app config, or session
storage. Its command surface is `hya update …` on the unified `hya` executable
(the standalone `hya-updater` binary was removed in 0.38.0); `hya` dispatches
`update` before composing any runtime.

Production activation requires an explicit capability issued by the trusted
updater owner. The capability binds the exact candidate release sequence and
the expected active generation; activation rejects a wrong owner token, stale
generation, or a different signed sequence even when its release sequence is
higher. The updater-root OS lease serializes staging, activation, recovery, and
discard. A signature is necessary but not sufficient. Staged-only applies
remain available. The capability is a trusted-filesystem handoff, not a
same-UID security boundary: processes able to write the updater root are in the
same trust domain and must be protected by host ownership/permissions.
Network download is **outside** the TCB; download a complete package directory
first, then verify/stage/activate.

`install.sh` remains break-glass bootstrap and manual recovery. Bare
`hya update` (no subcommand) is a different, non-TCB path: it reinstalls from
the published GitHub release, checked against its `SHA256SUMS`
([install.md](install.md)).

## Rebuild and restart (source checkout)

`hya update` installs signed releases. Code you (or an agent) change in a
source checkout reaches the running backend without it: build, then restart
the daemon from the new build. The restart proves the build first (`hya serve
check` against a snapshot of the live database, then your `--verify`
commands), hands running root turns to the new build so the next turn runs
the new code, and rolls back to the pinned previous build if the new one fails
to start. Details: [cli.md, Self-proof and rollback](cli.md#self-proof-and-rollback).

```sh
cargo build -p hya-backend --bin hya
./target/debug/hya serve restart --verify 'cargo test -p hya-core'
```

## Layout under an updater root

```text
<root>/
  trust_roots.json      # ed25519 verifying keys (TCB)
  accepted_floor        # monotonic accepted sequence
  current               # active generation selector
  generation            # durable monotonic active-generation fence
  authorization.json    # latest trusted owner-issued capability (CAS record)
  updater.lock          # OS-backed updater-root lease (flock on Unix)
  activation.journal    # prepare/commit/abort + owner token/generation

Control files must never live under `releases/`. Session databases and secrets
must not appear under the updater root. The trusted owner issues the
capability with `hya update authorize --root … --sequence N --out FILE`: it
takes the root lease (`UpdaterOwner`), binds release `N` to the active
generation (`authorize`), and writes the JSON (`write_authorization`). At a
terminal it asks for confirmation; without one it refuses unless `--yes` (for
the owner's own supervisor). Agents never issue it. Same-UID processes that
can write this root share the trust boundary; the capability is not a sandbox.

## CLI

Build:

```sh
cargo build -p hya-backend --bin hya
```

Commands:

```sh
# Inspect
./target/debug/hya update version
./target/debug/hya update status --root /var/lib/hya/updater

# Recover interrupted prepare/commit
./target/debug/hya update recover --root /var/lib/hya/updater

# Stage only (default product path without owner gate)
./target/debug/hya update apply \
  --root /var/lib/hya/updater \
  --metadata ./release.metadata.json \
  --package ./package-dir \
  --platform x86_64-unknown-linux-gnu \
  --smoke smoke.sh

# The owner issues the capability (confirms at the terminal), which releases
# its lease on exit; the updater validates the supplied JSON against
# root/authorization.json before activation.
./target/debug/hya update authorize \
  --root /var/lib/hya/updater \
  --sequence 42 \
  --out ./activation.authorization.json
./target/debug/hya update apply \
  --root /var/lib/hya/updater \
  --metadata ./release.metadata.json \
  --package ./package-dir \
  --platform x86_64-unknown-linux-gnu \
  --smoke smoke.sh \
  --authorization ./activation.authorization.json

# Optional external trust roots remain compatible with the same handoff.
./target/debug/hya update apply \
  --root /var/lib/hya/updater \
  --metadata ./release.metadata.json \
  --package ./package-dir \
  --platform x86_64-unknown-linux-gnu \
  --trust-roots /secure/media/trust_roots.json \
  --authorization ./activation.authorization.json

# Discard a staged-but-not-accepted candidate
./target/debug/hya update discard --root /var/lib/hya/updater --sequence 42
```

Bootstrap trust roots (operator only):

```sh
./target/debug/hya update init-roots \
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
| `--authorization <PATH>` | Owner capability JSON bound to exact candidate sequence and expected active generation. Omit for stage-only. |
| `--trust-roots <PATH>` | Override path to `trust_roots.json` (default: `<root>/trust_roots.json`). Use when keys live on separate/read-only media or when verifying against a staged key set during rotation. |

### Verification gate chain (`apply`)

`verify_release_metadata` runs gates **in this order**. The first failure is
what the operator sees:

1. **`protocol_version`** — must equal the supported value (`1`).
2. **`min_updater_version`** — compared to the updater's version (the backend
   release version, `[backend].version` in `versions.toml`) with
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
