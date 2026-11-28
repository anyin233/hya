# Migrating from Hya v1 to v2 (`hya-migration`)

## 1. Introduction

Hya v2 ([ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)) is a clean-sheet modular release: the runtime speaks only `hya.v2`, `hya-store` enforces SQLite `format_version = 2`, `config.yaml` requires `schema_version: 2`, `hyabundle` accepts `schema_version: 2` only, and `packages/hya-tui` requires `tui.json` `paneLayout` `version: 4` and `@hya/tui-sdk` v2 (`api_version: 2`).

Rather than carrying legacy v1 compatibility shims inside the runtime crates, **100% of v1-to-v2 upgrade logic is isolated in the standalone `hya-migration` CLI binary** (`crates/hya-migration`, packaged alongside `bin/hya` in release archives).

If you open an unmigrated v1 database or config with Hya v2, `hya` fails fast with an explicit error directing you to run `hya-migration`:

- **Read-only preflight (`hya-migration check`):** Scans `sessions.db`, `config.yaml`, `tui.toml`, `tui.json`, `registry.sqlite3`, and `.hya/bundles/` without modifying any file.
- **Atomic SHA-256-backed upgrade (`hya-migration run`):** Snapshots every affected file (including SQLite `-wal`/`-shm` sidecars) with a lowercase hex SHA-256 `manifest.json` *before* touching disk, performs lossless atomic temp-file swaps, verifies `PRAGMA foreign_key_check`, `PRAGMA integrity_check`, and v2 `SessionStore` readiness, and validates upgraded bundle manifests with `hya-bundle`.
- **Byte-exact verified rollback (`hya-migration rollback`):** Re-hashes every backed-up file against `manifest.json`, restores every original file atomically, and removes newly created files (such as extracted per-bundle `config.yml` files).

### Breaking-change checklist (v1 → v2)

| Surface | Hya v1 | Hya v2 (required) | Handled by `hya-migration`? |
| --- | --- | --- | --- |
| **SQLite `sessions.db`** | Legacy `event_log` domain event variants + `resident_actor_claim` table | `schema_meta.format_version = 2`; 26 domain event tags translated to `Event::Extension`; `resident_actor_claim` migrated to `bundle_claim` (+ metadata in `bundle_state`) | Yes (lossless copy + integrity & `SessionStore` verification) |
| **`config.yaml`** | Unversioned / `schema_version: 1`; inline `compaction`, `goal`, `subagents`, `formatter`, `lsp`, `websearch`, `webfetch`, `tools.{websearch,webfetch}`; legacy bundle IDs | `schema_version: 2`; `bundles.builtin.{profile,enable,disable}` with canonical v2 IDs; domain sections extracted to `<config-dir>/bundles/<percent-encoded-id>/config.yml` | Yes (atomic write + merge into existing per-bundle `config.yml`) |
| **`tui.toml`** | `version = 1`; top-level or preset `[subagents]` / `[workflows]` sections; legacy layout pane kinds | `version = 2`; `[panes."hya/multi-agent#subagents"]` / `[panes."hya/workflow#workflows"]`; dynamic extension pane keys in `[layout]` | Yes |
| **`tui.json`** | Binary `paneLayout` `version: 1..=3` or hardcoded `subagents`/`subagent-viewer`/`workflows` leaves; legacy bundle IDs in `extensionEnabled`/`extensionTrusted` | `paneLayout` `version: 4` n-ary split tree with `{ kind: "extension", panel: "<bundle>#<panel>" }` leaves; canonical v2 bundle IDs | Yes |
| **User bundles & `registry.sqlite3`** | `schema_version: 1` (or unversioned); `extensions.*`; `exposure.yaml`; `version_ref: backend`; `abi_version: 1`; legacy disabled IDs in `registry.sqlite3` | `schema_version: 2` only; `runtime:` + `assets:`; `overrides: hya/bare` (no `exposure.yaml`); `version_ref: hya`; `abi_version: 2`; reserved kernel tools (`todo`, `rewrite`) forbidden | Manifests & `registry.sqlite3` migrated automatically; native ABI v1 `cdylib`s & custom JS/TUI code flagged for rebuild/reinstall |
| **Wire API & SDK** | `proto/hya/v1` (`/v1/*`), `crates/hya-sdk-v1`, core `Workflow` RPCs & `Session.{Archive,Steer}Subagent` | `proto/hya/v2` (`/v2/*`, 15 services / 95 RPCs), `crates/hya-sdk`, domain endpoints on `BundleApi`, domain events on `ExtensionEvent` | Client code update required (see **[`hya.v2` Protocol Overview](../protocol/README.md)**) |

---

## 2. Usage

### End-to-end migration & rollback walkthrough

> **Important:** Stop any running `hya serve` daemon before migrating `sessions.db` so no active writer holds the database lock or writes to the WAL while migrating.

```sh
# 1. Stop your running v1 daemon (if any)
hya serve stop

# 2. Inspect all default locations read-only
hya-migration check

# 3. Preview every planned file rewrite without modifying disk
hya-migration run --dry-run

# 4. Run the atomic migration (writes a SHA-256 backup manifest first!)
hya-migration run --backup-dir ~/.config/hya/migration-backups/pre-v2

# Optional: inspect machine-readable JSON output for automation
hya-migration --json check

# 5. Start the v2 daemon and verify your migrated sessions & config
hya serve start
hya sessions
hya bundle list
```

If you need to revert to the exact pre-migration bytes at any time:

```sh
# Stop the v2 daemon first so it does not hold sessions.db open
hya serve stop

# Verify every backed-up file's SHA-256 digest and restore originals atomically
hya-migration rollback --backup ~/.config/hya/migration-backups/pre-v2
```

### Migrating project bundles or non-default paths

Override any target path explicitly (repeat `--bundles-dir` for multiple repositories):

```sh
hya-migration run \
  --db /srv/hya/sessions.db \
  --config /srv/hya/config.yaml \
  --registry /srv/hya/bundles/registry.sqlite3 \
  --bundles-dir ~/repos/app-a/.hya/bundles \
  --bundles-dir ~/repos/app-b/.hya/bundles \
  --backup-dir /srv/hya/backups/v1-to-v2
```

### CLI Reference (`hya-migration`)

```text
hya-migration [--json] <COMMAND>
```

| Command / Option | Default | Description |
| --- | --- | --- |
| `--json` *(global)* | `false` | Emit structured JSON (`CheckReport`, `RunReport`, or `RollbackReport`) on `stdout` instead of human-readable text. |
| `check [TARGET_ARGS]` | — | Read-only inspection of `sessions.db`, `config.yaml`, `tui.toml`, `tui.json`, `registry.sqlite3`, and `--bundles-dir` manifests. Never modifies disk (copies SQLite to a private temp staging copy before reading). |
| `run [TARGET_ARGS] [--dry-run] [--backup-dir <DIR>]` | Auto backup dir | Creates a SHA-256 backup snapshot (when `--dry-run` is not set and at least one artifact needs migration), then atomically upgrades all v1 artifacts to v2. |
| `rollback --backup <DIR>` | Required | Verifies every file in `<DIR>/manifest.json` against its recorded SHA-256 digest and byte size, atomically restores originals, and deletes files created during migration. (`--backup-dir` is an alias for `--backup`.) |

#### Target path options (`TARGET_ARGS`, shared by `check` and `run`)

| Flag | Default resolution |
| --- | --- |
| `--db <PATH>` | `$XDG_STATE_HOME/hya/sessions.db` or `~/.local/state/hya/sessions.db` |
| `--config <PATH>` | `$XDG_CONFIG_HOME/hya/config.yaml` or `~/.config/hya/config.yaml` *(setting `--config` also anchors default `tui.toml`/`tui.json` to that parent directory unless overridden)* |
| `--tui-toml <PATH>` | `$HYA_TUI_TOML`, else `<dir-of-tui.json>/tui.toml` |
| `--tui-json <PATH>` | `$HYA_TUI_CONFIG`, else `<config-dir>/tui.json` |
| `--registry <PATH>` | `$XDG_DATA_HOME/hya/bundles/registry.sqlite3` or `~/.local/share/hya/bundles/registry.sqlite3` |
| `--bundles-dir <DIR>` | Repeatable. Defaults to `./.hya/bundles` when present in the current working directory. Walks up to 4 levels deep for `bundle.yaml` / `bundle.hya.md` (skipping dotfiles, `node_modules`, and `target`). |

**Default backup directory:** When `--backup-dir` is omitted on `hya-migration run`, snapshots are written to `<config-dir>/migration-backups/v1-to-v2-<unix-epoch-ms>/`. An existing backup directory containing `manifest.json` is rejected so prior backups can never be overwritten.

---

## 3. Interface Definitions & Mapping Tables

### 3.1 SHA-256 Backup Manifest Schema (`manifest.json`)

Written atomically before any target file is mutated. Backed-up file bytes live under `<backup-dir>/files/<04d>_<sanitized_filename>`.

```json
{
  "schema_version": 1,
  "created_at": "2026-11-28T06:00:00Z",
  "tool_version": "0.52.0",
  "entries": [
    {
      "original_path": "/home/alice/.local/state/hya/sessions.db",
      "backup_rel_path": "files/0000_sessions.db",
      "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
      "size_bytes": 131072,
      "existed": true,
      "kind": "sqlite_db"
    },
    {
      "original_path": "/home/alice/.config/hya/bundles/hya%2Fnetwork/config.yml",
      "existed": false,
      "kind": "bundle_config"
    }
  ]
}
```

| Field | Type | Description |
| --- | --- | --- |
| `schema_version` | `u32` | Always `1` (`BACKUP_MANIFEST_VERSION`). |
| `created_at` | `string` | UTC RFC-3339 timestamp. |
| `tool_version` | `string` | `hya_version::VERSION` that created the snapshot. |
| `entries[].original_path` | `string` | Canonical absolute path of the live artifact on disk. |
| `entries[].backup_rel_path` | `string?` | Relative path inside `<backup-dir>` when `existed == true`. |
| `entries[].sha256` | `string?` | 64-character lowercase hex SHA-256 digest of the original bytes (`existed == true`). |
| `entries[].size_bytes` | `u64?` | Original byte length (`existed == true`). |
| `entries[].existed` | `bool` | `true` restores original bytes on rollback; `false` deletes the newly created file (and cleans empty newly created bundle config dirs) on rollback. |
| `entries[].kind` | `ArtifactKind` | One of: `sqlite_db`, `sqlite_sidecar`, `config_yaml`, `bundle_config`, `tui_toml`, `tui_json`, `bundle_registry`, `bundle_manifest`. |

### 3.2 SQLite `event_log` Domain Event Mapping (`v1` → `Event::Extension`)

Every v1 domain event JSON row `{ "type": "<legacy_tag>", "session": "<id>", ...fields }` is rewritten into v2 `Event::Extension`:

```json
{
  "type": "extension",
  "session": "<session_id>",
  "bundle": "<target_bundle_id>",
  "kind": "<legacy_tag>",
  "payload": { "...remaining fields..." }
}
```

| Target v2 Bundle ID | Rewritten v1 Event `"type"` Tags (26 total) |
| --- | --- |
| `hya/compaction` | `context_evicted`, `context_status` |
| `hya/workflow` | `workflow_selected`, `workflow_run_started`, `workflow_stage_started`, `workflow_stage_member_linked`, `workflow_stage_route_outcome`, `workflow_stage_finished`, `workflow_run_finished` |
| `hya/agent-channels` | `mail_sent`, `channel_joined`, `channel_created`, `mail_consumed` |
| `hya/multi-agent` | `session_status`, `member_spawned`, `member_status_changed`, `member_finished`, `agent_registered`, `agent_activity_changed`, `resident_work_started`, `resident_work_finished`, `agent_heartbeat`, `subagent_reported`, `handoff_committed`, `agent_archived`, `agent_restarted` |

Core kernel events (`session_started`, `message_started`, `message_finished`, `part_*`, `tool_*`, `todos_updated`, `tokens_recorded`, `context_compacted`, etc.) are preserved verbatim.

**Table & integrity migration:**
- Builds a fresh v2 database using `hya_store::SessionStore::connect` so `_sqlx_migrations` carries the exact v2 checksum and `schema_meta.format_version = '2'`.
- Copies `project`, `project_root`, `session`, `event_log` (with rewritten payloads), `pending_resume`, `saved_permission`, `file_blob` (and drops stale v1 `projection_snapshot` caches so v2 projections re-fold cleanly).
- Migrates `resident_actor_claim` rows into `bundle_claim` (under `bundle = 'hya/multi-agent'`, preserving fencing epochs) and records migration metadata in `bundle_state`.
- Runs `PRAGMA foreign_key_check`, `PRAGMA integrity_check`, and `hya_store::SessionStore::connect_read_only` before atomically swapping the temp DB onto `sessions.db` and removing stale `-wal`/`-shm` sidecars.

### 3.3 Legacy Built-in Bundle ID Mapping (`bundle_ids.rs`)

Used when rewriting `config.yaml` (`bundles.builtin.{enable,disable}`), `tui.json` (`extensionEnabled`, `extensionTrusted`), and `registry.sqlite3` (`bundle_disabled`):

| Legacy v1 ID (bare or `hya/`-prefixed) | v2 Canonical ID(s) / Action |
| --- | --- |
| `base-tools`, `hya/base-tools` | `["hya/hashline", "hya/coding-extras"]` |
| `extended-tools`, `hya/extended-tools` | `["hya/coding-extras", "hya/plan", "hya/workflow", "hya/multi-agent"]` |
| `network-tools`, `hya/network-tools` | `["hya/network"]` |
| `channel-tools`, `hya/channel-tools` | `["hya/agent-channels"]` |
| `goal-loop`, `hya/goal-loop` | `["hya/goal", "hya/loop"]` |
| `plan-impl-review`, `hya/plan-impl-review` | `["hya/workflow"]` |
| `code-mode`, `hya/code-mode` | **Removed (promoted to `hya-bare` core)** — configure via `tools.code.mode` |
| `compaction` | `["hya/compaction"]` |
| `core-agents` | `["hya/core-agents"]` |
| `core-skills` | `["hya/core-skills"]` |
| `core-commands` | `["hya/core-commands"]` |
| `agent-channels` | `["hya/agent-channels"]` |
| `basic-tui-components` | `["hya/basic-tui-components"]` |

### 3.4 `config.yaml` Domain Section Extraction

Every bundle config leaf is percent-encoded (`/` → `%2F`, matching `hya_app::bundle_config::bundle_config_location`):

| Legacy `config.yaml` Section | Target v2 Per-Bundle File | Section Key Written |
| --- | --- | --- |
| `compaction` | `<config-dir>/bundles/hya%2Fcompaction/config.yml` | `compaction` |
| `goal` | `<config-dir>/bundles/hya%2Fgoal/config.yml` | `goal` |
| `subagents` | `<config-dir>/bundles/hya%2Fmulti-agent/config.yml` | `subagents` |
| `formatter` | `<config-dir>/bundles/hya%2Fcoding-extras/config.yml` | `formatter` |
| `lsp` | `<config-dir>/bundles/hya%2Fcoding-extras/config.yml` | `lsp` |
| `websearch`, `tools.websearch` | `<config-dir>/bundles/hya%2Fnetwork/config.yml` | `websearch` |
| `webfetch`, `tools.webfetch` | `<config-dir>/bundles/hya%2Fnetwork/config.yml` | `webfetch` |

Remaining `config.yaml` keys (`default_model`, `default_agent`, `reasoning`, `providers`, `agents`, `categories`, `permission`, `mcp`, `plugins`, `tools.code`, `provider_retry`, `catalog_scopes`) stay in `config.yaml` with `schema_version: 2` emitted at the top and a default `bundles.builtin: { profile: full, enable: [], disable: [] }` block added if absent.

### 3.5 TUI Configuration & Saved Layout Mapping (`tui.toml` & `tui.json`)

| Artifact | v1 Shape | v2 Shape |
| --- | --- | --- |
| `tui.toml` `version` | `version = 1` (or missing) | `version = 2` |
| `tui.toml` `[subagents]` (top-level & `[presets.<name>.subagents]`) | `[subagents]` | `[panes."hya/multi-agent#subagents"]` |
| `tui.toml` `[workflows]` (top-level & `[presets.<name>.workflows]`) | `[workflows]` | `[panes."hya/workflow#workflows"]` |
| `tui.toml` `[layout]` pane kind strings | `"subagents"`, `"subagent-viewer"`, `"workflows"` | `"hya/multi-agent#subagents"`, `"hya/multi-agent#subagent-viewer"`, `"hya/workflow#workflows"` |
| `tui.json` `paneLayout` version | Binary split tree `version: 1`, `2`, or `3` | N-ary split tree `version: 4` |
| `tui.json` `paneLayout` leaf `kind` | `{ "type": "pane", "kind": "subagents" \| "subagent-viewer" \| "workflows" }` | `{ "type": "pane", "kind": "extension", "panel": "hya/multi-agent#subagents" \| "hya/multi-agent#subagent-viewer" \| "hya/workflow#workflows" }` |
| `tui.json` `extensionEnabled` / `extensionTrusted` | Legacy bundle ID keys | Remapped via the table in Section 3.3 |

### 3.6 User Bundle Manifests & `registry.sqlite3` Diagnostics

- **Automatic upgrades:** Sets `schema_version: 2`, normalizes `version_ref: backend` → `version_ref: hya`, translates legacy `extensions.{js, process, files, rust, libraries}` into `runtime:` + `assets:`, strips and removes legacy `exposure.yaml` (backed up in `manifest.json`), bumps `runtime.abi_version: 1` → `2`, and validates upgraded directories with `hya_bundle::prepare_package`.
- **Actionable warnings (`DiagnosticSeverity::Warning`):**
  - Removed `exposure.yaml` → replace with `overrides: hya/bare` on the bundle or tool entry if overriding core file primitives.
  - Native bundle upgraded from `abi_version: 1` → rebuild the native `cdylib` against `hya-runtime-abi` v2 (see **[Native Bundle Runtime ABI v2](../extend/native-abi.md)**).
  - Unreadable v1 rows in `registry.sqlite3` (`registry_rows_needing_reinstall > 0`) → upgrade the bundle source and reinstall with `hya bundle install`.
- **Blocking errors (`DiagnosticSeverity::Error`):** Any bundle declaring or aliasing the permanently kernel-exclusive tools `todo` or `rewrite` is flagged so you can rename or remove those tools before running `hya bundle verify --strict`.
