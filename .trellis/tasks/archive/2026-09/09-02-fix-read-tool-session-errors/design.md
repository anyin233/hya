# Technical Design

## Source Baselines

The behavior baseline is immutable:

- `pi-hashline-edit` 0.8.3, npm `gitHead`
  `ba7db9943d0f58499b24c1f6bd64722580f772a5`, npm tarball SHA-1
  `8985f24c3493be375cc225a5522ed54de8daabc9`.
- Oh My Pi `@oh-my-pi/pi-coding-agent` 18.1.3 at commit
  `0b769cc4dd9771373335430385d1d2f696dc3498`.

The npm package owns Read, Edit, and optional Grep. Oh My Pi owns the host Write,
Bash, result-envelope, and presentation behavior. The Rust port copies behavior,
not TypeScript runtime dependencies or framework code.

## Root Causes

### Read

The published Read schema contains both `filePath` and `path`, while
`ReadInput.path` uses `filePath` as a Serde alias. When both keys arrive, Serde
assigns one logical field twice and returns ``duplicate field `path` `` before
permissions or I/O. The live Session has 17 instances of this exact path.

### Task/Subagent

The Task schema advertises `inline_agent.description`, so the model emits an
empty string for it. Parsing turns that value into `Some("")`, while hya-app
rejects every `Some` value as an unsupported inline field before admission. The
schema and executable contract disagree in the same way as Read.

## Architectural Decision

Use one registry-owned native hashline runtime behind the four filesystem coding
tools. Keep Bash separate.

```text
ToolRegistry::builtins
  ├─ Arc<HashlineRuntime>
  │    ├─ ReadTool
  │    ├─ EditTool
  │    ├─ WriteTool
  │    └─ GrepTool
  └─ BashTool (canonical `bash`, hidden `shell` alias)

provider schema -> durable ToolCallRequested -> existing authorization
  -> tool-specific permission checks -> Rust implementation
  -> bounded { title, output, metadata } result
  -> durable ToolResult -> shared Projection -> Compat SDK ToolPart
  -> hya-owned OpenTUI coding-tool presentation
```

`HashlineRuntime` is private to `hya-tool`. `ToolCtx`, permission planes, Events,
Projection, store, and Compat DTOs remain the stable cross-layer contracts. The
registry creates one runtime and injects it into Read/Edit/Write/Grep so a
Read/Grep/Write and later Edit share the same recovery state.

## Native Module Boundaries

Add a private `crates/hya-tool/src/hashline/` module with a small facade and
focused internal modules:

- `mod.rs`: fixed defaults, `HashlineRuntime`, scope/target keys, adapter-facing
  read/edit/snapshot operations, and result limits.
- `hash.rs`: JavaScript-compatible line-end normalization, incremental XXH32,
  nibble alphabet encoding, hashline formatting, anchor parsing, and fresh
  range formatting.
- `apply.rs`: normalized edit request types, reference validation, hint
  forgiveness, span resolution, deduplication/conflict checks, descending
  application, changed-range calculation, warnings, and stable error codes.
- `state.rs`: bounded Session-scoped snapshot LRU, applied-payload guard, no-op
  counter, and fixed sharded async mutation locks.
- `merge.rs`: exact historical replay and context-3/fuzz-0 patch application.
- `fs.rs`: text classification/normalization, line-ending/BOM metadata,
  symlink resolution, hard-link-aware writes, same-directory temp replacement,
  mode preservation, fsync, and temp cleanup.

Keep adapters thin:

- `read.rs` owns schema, path compatibility, directory/media dispatch, permission
  order, and JSON result shaping.
- `edit.rs` owns schema/argument preparation, edit permission order, formatter,
  LSP, final diff metadata, and mapping hashline errors to `ToolError`.
- Move `GrepTool` from `tool.rs` to `grep.rs`; it owns schema, permissions,
  ignore-aware traversal, cancellation, and result shaping.
- `write.rs` owns the host Write contract and reuses the atomic writer and
  snapshot update seam.
- `shell.rs` becomes the canonical Bash adapter and does not depend on hashline.

Delete `edit_replace.rs` and `edit_replace/replacers.rs` after all callers move.
Do not keep a second fuzzy edit implementation or compatibility alias for the
old public edit surface.

Every new function has a purpose/parameter/return doc comment. Comments explain
invariants and divergences, not line-by-line mechanics.

## Hashline Core Contract

### Text And Hashing

1. Decode text, remove one leading UTF-8 BOM, and normalize CRLF/lone CR to LF.
2. Exclude the terminal empty sentinel caused by a final newline from visible
   rows.
3. For visible line `i`, normalize immediate previous/current/next lines by
   removing CR and applying ECMAScript-compatible trailing-whitespace removal.
4. Feed `prev`, NUL, `curr`, NUL, and `next` incrementally to XXH32 seed 0. Do
   not allocate a concatenated context string.
5. Keep the low 8/12/16 bits and encode most-significant nibble first with
   `ZPMQVRWSNKTXJBYH`.
6. Render `LINE#HASH:content`, padding only the decimal line-number column.

Use a native Rust XXH32 crate with the required algorithm feature. Keep the
exact package vectors as golden tests. Hashes are stale-reference aids, not
integrity or authorization data.

### Parsing And Applying

Parse optional whitespace and `>>>`/`+`/`-` markers, a positive line, `#`, the
configured 2..=4 alphabet characters, and an optional text hint. Validate every
anchor against the same pre-edit content before applying any span.

The four operations map to original-text spans:

- `replace(pos,end?,lines)` replaces an inclusive row range; empty `lines`
  deletes it.
- `append(pos?,lines)` inserts after an anchor or at EOF.
- `prepend(pos?,lines)` inserts before an anchor or at BOF.
- `replace_text(oldText,newText)` requires exactly one exact occurrence.

Resolve and validate all spans first, reject conflicting boundaries, deduplicate
identical spans, sort by descending end position with replacements before
insertions, pre-compute final capacity, then assemble the output once. Preserve
terminal newline behavior and reject making a non-empty file byte-empty.

Stable package error prefixes remain model-visible. Structural and anchor errors
map to `ToolError::Input`; permissions remain `ToolError::Permission`; I/O stays
I/O; post-write integration failures include the mutated path and operation
context.

### State And Recovery

State keys are `(SessionId | no-session workdir, normalized workdir,
resolved-target path)`. This is a deliberate security tightening over the
package's process-global singleton: a Session cannot recover content observed by
another Session.

Use `Arc<str>` snapshots with newest-first per-path versions and O(1) byte
accounting. Enforce the package limits globally within the runtime: eight target
entries, four versions per target, and 32 MiB total. Refresh an identical newest
snapshot instead of copying it. Hash normalized edit payloads for bounded guard
keys.

Use a fixed array of async lock shards selected from the full state key. It is
hard-bounded and serializes same-target mutations without an attacker-growing
lock map. A hash collision may serialize unrelated files but cannot mix their
state.

On direct stale-anchor failure only, replay the complete request against stored
versions newest-first. Build context-three hunks and apply only exact matches to
live content. Never fuzzy-relocate an anchor. First exact merge wins; conflicts
retain the original stale error plus the package recovery note.

Two identical no-op payloads return soft success; the third returns
`[E_NOOP_LOOP]`. Reject a repeated successful payload only while current content
still equals its stored post-edit snapshot. A non-raw Read clears that marker.

## Filesystem Mutation Contract

Permission checks use the existing lexical requested path. Only after those
checks does the writer resolve symlink chains for mutation/state identity. This
preserves current policy; changing symlink authorization is outside scope.

Hold the target lock across live read, validation/recovery, mutation, formatter,
BOM synchronization, final read, diff generation, and state update.

- Follow relative/absolute symlink chains up to 40 hops and reject cycles.
- For an existing inode with `nlink > 1`, truncate/write/fsync the inode in
  place so hard links retain identity.
- Otherwise create an exclusive same-directory mode-0600 temp file, copy the
  existing target's permission bits when replacing, write/fsync/close, rename,
  and remove a leftover temp on every error path.
- Preserve a leading BOM and predominant line ending. Mixed endings become the
  first detected style with a warning. Invalid UTF-8 is decoded with U+FFFD and
  a successful mutation rewrites valid UTF-8 with a warning.
- Run the existing formatter while the lock is held, restore the desired BOM,
  then re-read final bytes. Diffs, fresh anchors, display metadata, and snapshots
  describe this final state. LSP diagnostics follow and remain in metadata.

A formatter or LSP failure after mutation must state that the file changed; the
runtime still records the actual final snapshot before returning the contextual
error.

## Tool Schemas And Compatibility

### Read

The model schema exactly exposes:

```json
{
  "path": "string",
  "offset?": "integer >= 1",
  "limit?": "integer >= 1",
  "raw?": "boolean"
}
```

Runtime input uses distinct `path` and legacy `filePath` fields. Empty means zero
bytes; whitespace is a filename. Equal or sole non-empty values succeed,
conflicting non-empty values fail closed, and both missing/empty values fail.
Legacy `offset: 0` maps to line 1; the schema never advertises zero.

Text output follows package anchor/raw/truncation behavior. Hya directories and
attachments bypass hashing but retain existing result shapes and permissions.

### Edit

Expose the pinned `path + edits` discriminated schema with
`additionalProperties: false` on the top level and every variant. Port the
package's argument preparation for `file_path`, JSON-string `edits`, inferred
`replace_text`, and complete camel/snake old/new pairs. Reject ambiguous,
incomplete, mixed, or wrong-typed inputs. Do not expose or accept hya's old fuzzy
`filePath + oldString + newString` contract.

The model-visible `output` contains fresh anchors/warnings or the no-op message;
the host-only metadata retains the authoritative bounded diff, file-diff counts,
classification, warnings, diagnostics, and presentation facts.

### Grep

Expose `pattern`, `path`, `glob`, `ignoreCase`, `literal`, `context`, and `limit`
with pinned bounds/defaults. Use `ignore`/`globset`-style native traversal plus
Rust regex in a cancellable blocking worker; do not spawn `rg`. Observe one
extra match before marking truncation. Reload matched files through the shared
text loader, merge adjacent context ranges, format hashline rows, and record
snapshots only for successfully rendered files.

The JSON result keeps the package summary plus bounded
`metadata.display.groups[] = { path, rows[{line,text,isMatch}] }` for the TUI.

### Write

Expose the closed Oh My Pi base schema `{path, content}`. Strip a copied
hashline file header/line prefixes only when the complete content is
unambiguously a rendered hashline block; otherwise preserve bytes. Reuse the
atomic writer, formatter/LSP path, and final snapshot update. Add execute bits
for a leading shebang. Chmod failure is a non-fatal result warning, not a silent
catch. Return final display metadata so formatting cannot make the TUI preview
lie.

### Bash

`ShellTool::name()` becomes `bash`. Register it as the canonical command tool and
register `shell` only in the hidden alias map. Migrate all built-in resource
lists/docs/tests to `bash`; stale direct `shell` requests still dispatch.

Expose the closed base schema:

```json
{
  "command": "string",
  "env?": { "string": "string" },
  "timeout?": "number seconds",
  "cwd?": "string",
  "pty?": "boolean"
}
```

Validate finite non-negative timeout values. Default to 300 seconds; zero means
no deadline; clamp other values to 1..=3600 and report a clamp notice. Preserve
command permission before execution and external-directory permission for cwd.

For non-PTY execution, read stdout/stderr concurrently into one arrival-ordered
bounded sink. Keep at most 50 KiB inline; once crossed, create the existing hya
artifact and stream the complete output there without retaining it all in RAM.
For `pty: true`, use a Rust PTY library in a blocking worker with the same sink,
timeout, cancellation, process-tree termination, and result metadata. PTY
unavailability is an explicit input/runtime error, not silent non-PTY fallback.

Nonzero exit and timeout produce completed structured results with error/warning
metadata, as in Oh My Pi. Explicit cancellation remains `ToolError::Cancelled`.
Never echo `env` values.

## Result Envelope And Output Limits

Preserve the existing `{title, output, metadata}` JSON seam used by Compat, but
make the safety cap shape-aware and tool-specific:

- The default for unrelated builtin/MCP/plugin results remains 5,000 characters.
- Read/Grep/Bash may retain their already-bounded 50 KiB model output plus small
  metadata; Edit may additionally retain a separately bounded diff. Tool
  adapters declare a defaulted result-cap policy so external `Tool`
  implementations do not need a new required method.
- Coding-tool metadata has independent hard byte/row limits and explicit
  truncation flags. No adapter can use metadata to bypass a result cap.
- Provider replay prefers an object's string `output` field and falls back to
  JSON only for results without that field. This matches the plain-text upstream
  tool-result contract while durable Events still carry presentation metadata.

This change prevents the current global cap from converting a successful coding
result into an unstructured scalar and losing the title/diff/display metadata.
Backend truncation remains distinct from reversible TUI collapse.

## OpenTUI Presentation Boundary

Add `packages/hya-tui-ts/src/hya/coding-tool-presentation.tsx`. It has two deep
entry points:

```ts
presentCodingTool(part: ToolPart): CodingToolView | undefined
CodingToolPresentation(props): JSX.Element
```

`presentCodingTool` validates unknown input/metadata and returns an allowlisted
view union for Read file/directory, Write file, Edit diff, Grep groups, and
Bash/Shell output. It never copies arbitrary keys or environment values. Bad or
compacted data returns `undefined` and uses the existing inline/error fallback.

`CodingToolPresentation` owns only local collapse state and uses existing theme,
path, and width contexts:

- Read/Write: titled `<line_number><code>` with file grammar, correct source
  offset, wrapping, and styled truncation state.
- Edit: current `<diff>` behavior; split only above 120 columns unless stacked is
  selected, unified at 80 columns.
- Grep: file title plus numbered match/context rows; derive grammar per file,
  keep match identity visible, clip/wrap within the transcript.
- Bash/Shell: highlighted shell command followed by a titled plain output
  section and status row. ANSI is stripped from output; env is never rendered.

Keep the retained upstream Session route change to one dispatch call and delete
the superseded five completed-render branches. Initial hydration and
`message.part.updated` continue to converge through `SyncProvider`; the component
must not call transport APIs.

## Task/Subagent Fix

Keep `InlineAgentInput.description` as a hidden parser field so stale/direct
non-empty values can still reach the existing typed rejection. In
`into_inline`, convert exactly empty or whitespace-only description to `None`;
remove `description` from both nested model schema branches. Do not normalize a
non-empty value away. A red process E2E uses the captured all-default inline
object and proves child creation plus parent resumption.

## Review Hardening Boundaries

Permission admission is lexical and precedes filesystem observation. Read and
Grep derive one external-directory wildcard from the resolved lexical request;
target kind may select execution only after permission succeeds.

Mutation preparation records the target identity used to select regular versus
hard-link commit. The commit path compares that identity immediately before
rename or truncate/open. A mismatch is a non-committed error. Once bytes commit,
every awaited formatter/restore/reload/LSP stage checks cancellation through the
same reconciliation path that reloads and records the actual final snapshot.

Every boundary owns its budget once. Grep streams and discards an overlong
logical line without growing its retained buffer, ignore parsing is capped
before rule retention, and coding-result fitting serializes each nested row or
group exactly once. The engine reapplies the shape-aware cap after hooks, at the
last point before durable publication.

Bash artifact ownership starts before the first write and ends only when a
successful result publishes `outputPath`; the armed owner removes partial files
on all other exits. PTY completion waits for output EOF rather than treating the
leader exit alone as terminal, so deadline/cancellation still kills and reaps
the full process group.

SyncProvider remains the only network/store owner. The coding-tool adapter is a
pure projection of synchronized parts. Release packaging uses recursive source
copy plus explicit notice/dependency invariants rather than a maintained file
allowlist.

## Test Design

Follow independent red-green slices:

1. Task schema and exact captured empty-description spawn.
2. Hash/format/parser pure golden vectors.
3. Read schema, compatibility matrix, text/raw/truncation, media/directory, and
   exact four-key engine/process round trip.
4. Edit operation/error/conflict/no-op/recovery/atomic target matrix plus
   formatter/LSP/final-state metadata.
5. Grep schema/search/context/limit/cancellation/snapshot behavior.
6. Write schema/atomic/prefix/shebang/final preview and Bash
   schema/timeout/PTY/exit/cancel/truncation/artifact behavior.
7. Provider replay and shape-preserving result cap.
8. Pure TUI normalization, OpenTUI styled-span rendering at 80/140 columns,
   SyncProvider replacement, and real backend+hya-ts PTY live/replay behavior.

Port high-value vectors from the pinned package tests and record provenance in
test-module comments. Tests assert observable behavior and stable error codes,
not implementation structure.

## Compatibility And Migration

- Canonical model path changes from `filePath` to `path`; hidden Read-only
  compatibility handles the observed live transcript.
- Canonical command name changes from `shell` to `bash`; hidden dispatch keeps
  stale callers functional, while schemas/resource lists/docs migrate fully.
- Edit and Grep are intentional clean cutovers. All repository fixtures,
  prompts, agent resources, docs, and tests migrate in the same change.
- Historical ToolError Events remain immutable. Replay shows old failures; only
  new calls against a restarted 0.36.9 backend use the new code.
- Hashline recovery state is process-local and is lost on restart. Current-file
  anchor validation remains available after restart.

## Risks And Controls

- Two-character hashes have collision risk. Text hints veto collisions; hashes
  never authorize access.
- Source-derived behavior can drift. Golden fixtures, stable error matrices, and
  the immutable source pins are the conformance oracle.
- Large content can grow memory/events. Tool-specific byte/row limits, shared
  `Arc<str>` snapshots, hard LRU limits, bounded lock shards, and streaming Bash
  capture make every path bounded.
- Atomic replace can alter inode identity. Hard-linked files intentionally use
  in-place fsync; other files use same-directory rename, matching the source
  contract.
- Formatter mutation can stale anchors. Locks cover formatter execution and
  anchors are generated only from final bytes.
- Syntax parser downloads can fail. OpenTUI always falls back to readable plain
  text.
- The retained Session route is resync-sensitive. Keep one narrow adapter call
  and place hya-owned behavior under `src/hya`.

## Rollback

Each atomic commit is revertible in dependency order: presentation, host
Write/Bash, hashline adapters/runtime, then Task schema. No database migration is
introduced. Rolling back restores old execution for future calls but does not
alter persisted Events. Do not partially restore fuzzy Edit while leaving
hashline Read/Grep schemas published.
