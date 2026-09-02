# Error Handling

> How errors are handled in this project.

---

## Overview

- Libraries use typed `thiserror` enums and propagate errors with `?`; library
  code must not panic on runtime failures.
- Preserve typed errors across existing layer boundaries. Add the smallest
  variant needed at store/core/tool/spawn seams rather than copying an
  independent error stack.

---

## Error Types

- `StoreError::OperationIdConflict` is the durable immutable-request conflict
  and displays the stable code `OPERATION_ID_CONFLICT`.
- `StoreError::AdmissionTransitionConflict` rejects terminal rewrites.
- `StoreError::ActorAlreadyClaimed` distinguishes an ordinary competing claim;
  `StoreError::StaleActorClaim` rejects any old epoch/owner capability at the
  canonical mutation boundary.
- `SpawnError` distinguishes queue/admission overload, unavailable transport,
  operation conflict, and already-handled idempotent replay.
- `TaskTool` maps these into matching `ToolError` variants; engine tool-error
  payloads retain distinct machine-readable types.

- `ProviderDiscoveryOutcome` is provider-local and typed: discovered, empty,
  auth-required, auth-rejected, unsupported, or `CatalogFailure`. It contains no
  fallback model or raw response body.
- Credentialless 401/403 maps to `AuthRequired`; credentialed 401/403 maps to
  `AuthRejected`. Decode/schema/body/pagination failures are `invalid` status;
  URL/redirect/transport/timeout/other HTTP failures are `unavailable`.
- One provider failure must not abort mixed-provider startup or publish partial
  rows. The global all-zero-live result is the canonical offline snapshot.

---

## Error Handling Patterns

- Fail closed before child/session/effect creation when identity, fingerprint,
  persistence, or admission is uncertain.
- A duplicate identical operation is not redispatched. A conflicting duplicate
  is not mutated.
- Startup must return an error rather than expose spawn surfaces if admission
  recovery fails.
- Resident startup must remain closed if epoch takeover, running-work
  terminalization, or resident re-registration fails. A stale completion is a
  typed rejection and must not be converted to a successful no-op that wakes
  work or advances projection state.
- Logging is supplemental; do not log-and-continue past a failed pre-create
  safety gate.

---

## API Error Responses

- Operation identity remains internal in 0.34.4; do not add it to HTTP/proto/CLI
  payloads.
- Existing tool-result event JSON carries `{ "error": { "type", "message" } }`.
  Use `operation_id_conflict` and `operation_already_handled` for the new tool
  variants.

---

## Common Mistakes

- Do not collapse overload into unavailable or input errors.
- Do not treat an identical terminal replay as a second release.
- Do not convert an operation conflict into a fresh operation ID.
- Do not add `unwrap`/`expect` to library paths to satisfy tests.
---

## Scenario: Typed Coding-Tool Failures And Commit-Boundary Reconciliation

### 1. Scope / Trigger

- Trigger: changing native hashline Read/Edit/Grep, host Write/Bash, Task inline
  parsing, tool-result capping, event/projection replay, or the SDK/TUI boundary.
- Applies to the error handoff from `hya-tool` through `hya-core` and
  `hya-proto`, Compat SDK tool parts, provider replay, and the hya-owned
  OpenTUI presentation. This scenario supplements the execution contract in
  `backend/quality-guidelines.md`; it does not introduce a second error store.
- Source-derived hashline diagnostics follow `pi-hashline-edit` 0.8.3 at npm
  `gitHead ba7db9943d0f58499b24c1f6bd64722580f772a5` (tarball SHA-1
  `8985f24c3493be375cc225a5522ed54de8daabc9`).

### 2. Signatures

- `HashlineError { code: &'static str, message: String, hints: Vec<String> }`
  exposes `diagnostic()`. The bracketed stable code is outside the message
  budget; message text is capped at 8 KiB on a UTF-8 boundary with a
  content-free truncation marker; at most 16 hints are retained and each hint
  is capped at 512 bytes. Hashline code is a model diagnostic, not
  authorization data.
- Read failures enter `ReadRuntimeError::{Io, Hashline, Cancelled}` and Edit
  failures enter `MutationBeginError::{Io, Cancelled}` or
  `MutationWriteError::{Io, Committed { .. }}`. Native hashline failures and
  out-of-range Read offsets map through `ToolError::Input` with their stable
  code; filesystem failures remain `ToolError::Io`.
- `reconcile_after_commit(mutation, payload_digest, context, error)` reloads
  and records final bytes after a committed mutation. Cancellation observed
  after any committed formatter/restore/reload/LSP await uses
  `reconcile_cancelled_after_commit`, records the payload guard and actual
  snapshot, then returns `ToolError::Cancelled`.
- `ToolError` keeps `Input`, `Permission`, `Io`, `Json`, `Cancelled`,
  `Overloaded`, `OperationIdConflict`, `OperationAlreadyHandled`, `Other`, and
  `UnsupportedInlineAgentField` distinct. The engine serializes a failure as
  `{ "error": { "type": string, "message": string } }` in the existing
  `ToolError` event value; it must not replace one class with another.
- Model-facing schemas are closed: Read `{path,offset?,limit?,raw?}`; Edit
  `{path,edits}` with `replace|append|prepend|replace_text`; Grep
  `{pattern,path?,glob?,ignoreCase?,literal?,context?,limit?}`; Write
  `{path,content}`; Bash `{command,env?,timeout?,cwd?,pty?}`. `filePath` is
  hidden Read-only compatibility, `shell` is a hidden Bash alias, and Task's
  nested `inline_agent` has no advertised `description`.
- Edit's adapter may accept only the pinned `prepareArguments` parser boundary:
  `file_path`, JSON-string `edits`, inferred `replace_text`, and complete
  camel/snake old/new pairs. These inputs are hidden compatibility, not public
  schema fields; hya's fuzzy `filePath + oldString + newString` surface remains
  rejected.

### 3. Contracts

- Validate schema and compatibility at the owning adapter before permission,
  filesystem, child/session, or process side effects. Read resolves distinct
  canonical `path` and legacy `filePath` fields byte-for-byte: a sole non-empty
  value or equal pair succeeds; a conflicting pair or both absent/empty values
  returns `ToolError::Input`. Hidden Read `offset: 0` means line 1, but the
  published schema minimum is 1. Do not trim a path to make it valid.
- Map native failures without flattening them. The stable hashline input family
  includes `E_BAD_REQUEST`, `E_BAD_OP`, `E_BAD_REF`, `E_BAD_CONFIG`,
  `E_RANGE_OOB`, `E_STALE_ANCHOR`, `E_NO_MATCH`, `E_MULTI_MATCH`,
  `E_EDIT_CONFLICT`, `E_DUPLICATE_EDIT`, `E_NOOP_LOOP`, `E_WOULD_EMPTY`,
  `E_INVALID_PATCH`, `E_CAPACITY`, `E_OUTPUT_LIMIT`, `E_INTERNAL`, and
  `E_BAD_READ`. Preserve the code and bounded recovery hints in
  `ToolError::Input("[E_...] ...")`; never include file contents in a hint, log,
  or error payload.
- A stale Edit may recover only through exact context-3/fuzz-0 replay of the
  complete request against bounded Session/workdir/target snapshots. A
  conflict, missing history, or shifted live content retains the original
  stale failure plus a bounded recovery note. It never silently fuzzy-relocates
  an anchor. Snapshot state is process-local, capped at 8 targets, 4 versions
  per target, and 32 MiB, with fixed lock shards for same-target serialization.
- Permission failures remain `ToolError::Permission` and preserve lexical
  requested-path ordering. Read and Grep select one kind-blind external parent
  wildcard and authorize it before metadata, existence, target-kind, symlink,
  snapshot, or content observation. A denial must not reveal whether an
  external target is missing, a file, or a directory. Do not convert denial to
  `E_BAD_REF`, `Io`, or an empty success.
- Mutation preparation records the target identity. The commit boundary
  revalidates it immediately before ordinary rename or hard-link truncate/open;
  a pathname or alias swap returns a non-committed contextual I/O error and
  leaves the replacement inode untouched. After commit, the target lock remains
  held through formatter, BOM/line-ending restoration, LSP, final reload,
  diff/preview generation, and final snapshot recording. If any post-commit
  stage fails, `reconcile_after_commit` records the bytes that are actually
  present and returns contextual `ToolError::Other` beginning with
  `File changed at <path>` and naming the stage; if final reload also fails,
  that detail is appended. A successful final reconciliation returns
  `ToolError::Cancelled` when cancellation arrived after commit. Never report
  "not changed" after a committed write.
- Task's parser keeps nested `description` private only for stale/direct callers.
  Empty or whitespace-only text becomes `None` and proceeds to admission; a
  non-empty value remains `Some` and is rejected as
  `ToolError::UnsupportedInlineAgentField { field: "description" }` before any
  child, session, governor, or lifecycle side effect. Authorization, model /
  category precedence, resident behavior, and run-tree projection are unchanged.
- `ToolResult` retains `{title,output,metadata}` and `ToolError` retains its
  structured error value through `ToolPartState`. The unrelated result default
  remains 5,000 characters. Coding-tool output and metadata are independently
  bounded (50 KiB inline where the tool contract permits it), and Bash creates
  the existing complete artifact when inline output is truncated. Provider
  replay reads an object's string `output` first and JSON-falls back only when
  that field is absent. No cap may turn a structured coding result into an
  uncorrelated scalar.
- The TUI parser validates projected SDK `ToolPart` state once. Unknown keys,
  malformed metadata, pending/streaming state, permission/denied state, and
  unsupported completed shapes take the existing inline/error fallback. Syntax
  parser failure is readable plain text, not a render exception. This fallback
  is local presentation behavior; it must not rewrite the backend error or
  request the raw Event log.
- Historical `ToolError` Events remain immutable. Replay shows the stored error;
  only calls made after a running 0.36.8 backend is restarted can use the
  0.36.9 schema and native error mapping.

### 4. Validation & Error Matrix

| Failure point | Error and side-effect rule |
| --- | --- |
| Unknown schema key, wrong type, missing required field, or invalid compatibility pair | `ToolError::Input` (hashline `E_BAD_REQUEST`/`E_BAD_OP` where applicable); stop before permission/I/O |
| Read both paths conflict, or both path values are empty/missing | `ToolError::Input`; do not resolve either path or ask permission |
| Invalid hashline anchor/range/configuration or Read bounds | `ToolError::Input` retaining `E_BAD_REF`, `E_RANGE_OOB`, `E_BAD_CONFIG`, or `E_BAD_READ` |
| Direct stale anchor with exact stored recovery | Complete successfully with bounded recovery notice and fresh final anchors |
| Stale conflict/no history/changed live hunk | `E_STALE_ANCHOR` plus bounded recovery note; no mutation unless the exact merge succeeds |
| Duplicate edit, repeated applied payload, no-op loop, or would-empty result | `E_DUPLICATE_EDIT`, `E_NOOP_LOOP`, or `E_WOULD_EMPTY`; no unsafe second mutation |
| External-directory/tool permission denial | `ToolError::Permission`; preserve permission ordering and no state side effect |
| Filesystem read/write failure before commit | `ToolError::Io`; do not claim a committed mutation |
| Formatter/BOM/LSP/reload/preview failure after commit | `ToolError::Other` with `File changed at <path>` and stage context, after final snapshot reconciliation |
| Cancellation before commit | `ToolError::Cancelled`; no committed-state claim |
| Cancellation after commit with successful reload | `ToolError::Cancelled`; final bytes and snapshot are still recorded |
| Task blank nested description vs non-empty description | Blank becomes absent and proceeds; non-empty is typed unsupported-field rejection before admission |
| Spawn overload/conflict/already handled | Preserve `Overloaded`, `OperationIdConflict`, or `OperationAlreadyHandled`; never retry by minting a new operation |
| Bash zero/finite invalid timeout, nonzero exit, timeout, cancel, PTY unavailable | Zero disables; finite values clamp to `1..=3600`; nonzero/timeout are completed structured results; cancel is `Cancelled`; PTY unavailability is explicit |
| Oversized result or metadata | Bounded truncation/artifact with explicit marker; never unbounded memory or secret-bearing diagnostics |
| Malformed replayed ToolPart or syntax parser failure | TUI fallback/plain text; no second request and no historical Event rewrite |

### 5. Good / Base / Bad Cases

- Good: a Read with `{filePath:"a.txt",path:"a.txt",offset:0,limit:20}` reaches
  I/O and returns a correlated result; an Edit formatter then fails, but the
  user sees `File changed at ...`, and a subsequent Edit can use the recorded
  final snapshot rather than stale pre-format bytes.
- Base: an invalid anchor returns `[E_BAD_REF]`, an outside path returns a
  permission error, and a completed Bash timeout carries structured status;
  each remains distinguishable after projection, provider replay, and TUI
  fallback.
- Base: an empty Task nested description creates the child and lets its parent
  resume; a historical 0.36.8 duplicate-field error is still visible after
  replay and is not repaired in storage.
- Bad: `map_err(|_| ToolError::Other("tool failed"))`, logging the stale file
  body, returning success after a committed formatter failure, or recovering
  with a fuzzy nearest anchor.
- Bad: converting every result to a capped string, treating `shell` as a
  separately advertised tool, allowing non-empty Task descriptions to reach
  spawn, or making the TUI retry malformed metadata over HTTP.

### 6. Tests Required

- `crates/hya-tool/tests/read*.rs`: assert the closed schema, dual-path matrix,
  hidden offset-zero rule, positive bounds, permission ordering, and typed
  input errors before any filesystem effect.
- `crates/hya-tool` hashline tests: assert each listed stable code, bounded
  content-free diagnostics/hints, exact recovery success/conflict/no-history,
  duplicate/no-op guards, snapshot isolation/eviction, fixed-lock behavior, and
  cancellation before and after mutation.
- Edit integration tests inject formatter, BOM, LSP, reload, and preview
  failures after commit; assert `File changed at`, final snapshot availability,
  no false success, and final-state diff/anchors. Filesystem fixtures assert
  permissions, symlink/hard-link identity, line endings, BOM, and cleanup.
- Write/Bash tests assert closed schemas, result-cap shape preservation, Bash
  timeout/clamp/zero/cancel/nonzero/PTY/truncation/artifact behavior, and that
  environment values are absent from every result and diagnostic.
- `crates/hya-tool/tests/task.rs` and app admission tests assert `description`
  is absent from both nested schemas, blank normalization has no unsupported
  error or child-side-effect, and non-empty direct input has the typed rejection
  with no admission mutation.
- Provider conformance tests assert object `output` replay, JSON fallback, and
  preservation of `ToolError` type/message; core/server replay tests assert
  historical Events and projected error values are unchanged.
- TUI presentation tests assert malformed/unknown/denied/attachment/directory/
  truncation states use fallback, ANSI is stripped from Bash output, syntax
  failure remains plain text, and no transport call or Event replay is made.
  Render at 80 and 140 columns; a real backend PTY test closes/reopens the same
  Session and asserts the same completed error/success blocks.

### 7. Wrong vs Correct

#### Wrong

```rust
let write = mutation.commit(&prepared.desired).await?;
ctx.formatter.format_file(&workdir, mutation.target_path()).await?;
let preview = mutation.preview(&prepared.original, &prepared.desired)?;
```

The `?` path loses the commit boundary: a formatter failure can leave changed
bytes with no final snapshot, and the preview can describe bytes that were never
written. Collapsing every hashline failure into `ToolError::Other` also removes
the stable code needed by the model and tests.

#### Correct

```rust
let write = match mutation.commit(&prepared.desired).await {
    Ok(write) => write,
    Err(MutationWriteError::Committed { .. }) => {
        return Err(reconcile_after_commit(
            &mut mutation,
            prepared.payload_digest,
            "atomic write synchronization",
            "commit reported an integration failure".to_string(),
        ).await);
    }
    Err(MutationWriteError::Io(error)) => return Err(ToolError::Io(error)),
};
```

The adapter keeps the lock through final reload/formatter/LSP/preview work,
records final bytes on every committed failure, and maps private hashline codes
to `ToolError::Input` without exposing file contents. The projected SDK part and
TUI then preserve that typed outcome or use the bounded fallback without making
their own error state.
