# Session revert snapshot baseline design

## Scope

Branch: `feat/opencode-revert-snapshot-baseline`

Worktree: `.worktrees/opencode-revert-snapshot-baseline`

Assigned version: `0.29.6`

Primary files:

- `crates/hya-tool/src/edit.rs`
- `crates/hya-tool/tests/edit.rs`
- `crates/hya-server/src/compat/session_revert.rs`
- `crates/hya-server/src/compat/session_diff.rs` if shared target helpers are needed.
- `crates/hya-server/tests/compat_session_revert_api.rs`
- Release metadata files required by AGENTS.

## Snapshot format

For new edit tool results, add metadata fields under the existing `metadata.filediff` object:

```json
{
  "beforeContent": "old\n",
  "afterContent": "new\n"
}
```

The server should treat these as optional. Existing event logs without snapshots must still return the current revert metadata and diff response without writing files.

## Restore behavior

On `revert`, replay target tool results, collect the latest snapshot per normalized relative file, and write `beforeContent` back under the session workdir. On `unrevert`, write `afterContent`.

Path safety must reuse the existing workdir-relative normalization pattern and reject absolute paths that escape the workdir. Restoration should be best-effort per file but surface I/O errors as API errors rather than silently claiming success.

## Non-goals

- No full OpenCode patch stack, compaction checkpoint, or message pruning.
- No reverse unified-diff parser.
- No snapshots for every write tool in this first slice unless the worker can add them without widening risk; `edit` is the required acceptance baseline.
