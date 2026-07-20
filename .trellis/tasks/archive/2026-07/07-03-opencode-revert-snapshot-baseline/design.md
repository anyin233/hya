# Session revert snapshot baseline design

## Scope

Branch: `feat/opencode-revert-snapshot-baseline`

Base: `feat/opencode-tui-theme-picker`

Assigned version: `0.33.14`

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

The snapshot strings include the UTF-8 BOM when present and reflect the actual file after formatter execution. The tool-result `created` flag distinguishes a new file from an emptied existing file. The server treats these fields as optional; existing event logs without snapshots retain metadata-only behavior.

## Restore behavior

On `revert`, replay target tool results, retain the earliest before snapshot and latest after snapshot per normalized relative file, and write `beforeContent` back under the session workdir. A file created by the earliest matching edit is removed instead. On `unrevert`, write or recreate `afterContent`.

Path safety accepts both raw and canonical workdir prefixes, rejects parent traversal, and canonicalizes the nearest existing ancestor to prevent symlink escapes from the session workdir. This supports relative workdirs and macOS `/tmp` canonicalization without weakening containment. Restoration surfaces I/O errors as API errors rather than silently claiming success.

## Non-goals

- No full OpenCode patch stack, compaction checkpoint, or message pruning.
- No reverse unified-diff parser.
- No snapshots for every write tool in this first slice unless the worker can add them without widening risk; `edit` is the required acceptance baseline.
