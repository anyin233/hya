# Quality Guidelines

> Code quality standards for frontend development.

---

## Overview

The shipped TUI is `packages/hya-tui-ts`. Cover behavior with focused Bun tests
at the existing SDK, state, command, or rendering boundary. Prefer semantic
assertions over brittle full-screen snapshots.

Run from `packages/hya-tui-ts` after frontend changes:

```sh
bun run typecheck
bun test
```

Launcher or backend changes additionally require the Rust workspace gate:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## Forbidden Patterns

- Reintroducing a Rust TUI renderer or routing shipped interactive behavior
  outside `packages/hya-tui-ts`.
- Direct backend process discovery or spawning inside the TypeScript package.
- A second HTTP/SSE client or projection beside the existing SDK/sync contexts.
- Imports from excluded OpenCode backend, worker, updater, web, or desktop code.
- Raw color literals when an existing semantic theme role expresses the state.

---

## Required Patterns

- Write and run one focused failing test before changing frontend behavior.
- Preserve the `src/hya` and `src/upstream` ownership boundary.
- Reuse the existing Solid contexts, routes, command registry, and plugin slots.
- Keep backend state synchronized through `@opencode-ai/sdk/v2`.
- Preserve prompt visibility and readable state labels on narrow terminals.

---

## Testing Requirements

Every behavior change needs the smallest Bun test that fails without it.
Responsive changes should exercise narrow and wide terminal dimensions. Changes
to runtime preparation or release packaging must retain the installer and
archive smoke tests described below.

---

## Code Review Checklist

- Does the change remain in `packages/hya-tui-ts`, the sole interactive
  frontend implementation?
- Does the TUI remain readable at 80 columns?
- Are status labels understandable without color?
- Do new tests fail on the old behavior and pass on the new behavior?
- Does server state still flow through the SDK/sync contexts?
- Does hya-specific integration remain in `src/hya/` when it should not alter
  retained upstream behavior?
- If `tui-check` reports `borderMisaligned=true` on a capture with multiple
  independent valid frames, verify manually and track the durable fix upstream;
  do not patch an installed generated package cache.

---

## Scenario: Prepared Bun runtime excludes SDK server entrypoints

### 1. Scope / Trigger

- Trigger: installing or release-packaging `packages/hya-tui-ts`, or changing
  the pinned `@opencode-ai/sdk` version/layout.
- The TypeScript TUI is a client of hya's backend. It must not ship an unused
  SDK path capable of spawning an OpenCode server or TUI.

### 2. Signatures

- Prepare dependencies with `bun install --frozen-lockfile --production` in the
  staged runtime.
- Then run
  `bun packages/hya-tui-ts/scripts/prune-sdk-server.ts <runtime-directory>`.
- The staged runtime contains `package.json`, `bun.lock`, `bunfig.toml`,
  `tsconfig.json`, `LICENSE`, `UPSTREAM.md`, `src/`, and production
  `node_modules/`.

### 3. Contracts

- Retain `@opencode-ai/sdk`'s `./v2/client` export and map `./v2` directly to
  the same client target.
- Remove exports `.`, `./server`, and `./v2/server`, plus the eager
  `dist/index.*` / `dist/v2/index.*` barrels, `dist/server.*`,
  `dist/v2/server.*`, and the pinned server-only `dist/process.*` helpers.
- Installer and release packaging call the same pruning script after the locked
  production install. Do not maintain two pruning lists.
- The script fails when the pinned SDK no longer has the expected v2 client
  export or the remapped `@opencode-ai/sdk/v2` client cannot be imported; an
  SDK layout change requires review rather than silent fallback.

### 4. Validation & Error Matrix

- Missing runtime argument -> preparation fails.
- Missing SDK manifest or expected v2 client export -> preparation fails before
  placement/archive creation.
- Any server export/file remaining -> installer/release smoke fails.
- Missing client entrypoint or failed `@opencode-ai/sdk/v2` import -> runtime
  verification fails.
- Missing `bunfig.toml` or `tsconfig.json` -> the staged source build/runtime
  verification fails.
- Preparation failure during install -> rollback leaves the prior binaries and
  runtime intact.

### 5. Good/Base/Bad Cases

- Good: the prepared runtime imports `createOpencodeClient` from SDK v2 while
  no SDK server export or process launcher remains.
- Base: development `node_modules` may contain the complete pinned package;
  only the staged install/release runtime is pruned.
- Bad: copying production `node_modules` verbatim and claiming server code is
  excluded merely because the frontend never imports it.

### 6. Tests Required

- Installer fixture creates client, barrel, and server SDK files, then asserts
  the client leaf remains, eager/server/process files and exports are absent,
  runtime config is installed, and rollback works.
- Release smoke repeats the client-present/server-absent assertions against the
  extracted archive.
- Prepare one real locked runtime, import `@opencode-ai/sdk/v2`, and compile its
  actual `src/main.tsx` using the staged configuration.

### 7. Wrong vs Correct

#### Wrong

```sh
bun install --frozen-lockfile --production
mv "$runtime" "$install_dir"
```

#### Correct

```sh
bun install --frozen-lockfile --production
bun packages/hya-tui-ts/scripts/prune-sdk-server.ts "$runtime"
mv "$runtime" "$install_dir"
```

---

## Scenario: TypeScript run-tree decoding of omitted projection fields

### 1. Scope / Trigger

- Trigger: changes to Rust run-tree projection serialization or the TypeScript
  `parseRunTree` boundary used by the subagent roster.

### 2. Signatures

- `GET /session/{session_id}/tree` returns recursive nodes with an optional
  `member` object.
- `parseRunTree(value: unknown): RunTreeNode` owns validation and normalization.
- `member.summary` is omitted while its Rust projection value is empty;
  otherwise it is a string.

### 3. Contracts

- An omitted `member.summary` normalizes to `""` at `parseRunTree`.
- A present `member.summary` must remain a string; do not coerce malformed
  values or weaken validation for other member fields.
- Consumers use the parsed `RunTreeNode`; roster rendering must not maintain a
  second decoder for the same response.

### 4. Validation & Error Matrix

- Missing `member.summary` -> parsed `summary: ""`.
- String `member.summary` -> preserve the string.
- `null`, number, boolean, array, or object `member.summary` ->
  `RunTreeParseError` at the `.member.summary` path.

### 5. Good/Base/Bad Cases

- Good: a running child omits `summary` and appears in the live roster.
- Base: a completed child supplies a string summary unchanged.
- Bad: requiring `summary` unconditionally rejects valid active projections;
  accepting `String(value)` hides malformed server responses.

### 6. Tests Required

- Parser test: an active member with omitted `summary` yields `""`.
- Parser test: the same member with a present non-string `summary` throws
  `RunTreeParseError`.
- Integration test: the real-backend subagent tree remains consumable through
  the pinned SDK workflow.

### 7. Wrong vs Correct

#### Wrong

```typescript
summary: string(input.summary, `${path}.summary`),
```

#### Correct

```typescript
summary: optionalString(input.summary, `${path}.summary`) ?? "",
```

---

## Scenario: Returning from a subagent observation hydrates pending interactions

### 1. Scope / Trigger

- Trigger: changes to read-only subagent panes, Main-pane focus ownership,
  descendant Permission or Question presentation, Session bootstrap/cache
  lookups, or global event recovery.

### 2. Signatures

- `focusMainPromptOwnership(...)` synchronously focuses the Main workspace and
  prompt.
- `compareSessionIDs(left, right)` applies JavaScript code-unit ordering.
- `sortSessionsByID(sessions)` and `sync.session.get(sessionID)` use that exact
  comparator for array ordering and binary search.
- `sdk.client.permission.list()` and `sdk.client.question.list()` return the
  current pending interactions across Sessions.
- `sync.set("permission" | "question", sessionID, rows)` updates the existing
  synchronized stores used by the Session route.

### 3. Contracts

- A subagent observation is read-only. Pending descendant interactions render
  only after focus returns to Main.
- The Session cache is a binary-search array ordered by `compareSessionIDs`.
  Every whole-cache ingestion path, including `/tui/bootstrap` bundles and
  multi-call Session lists, must normalize with `sortSessionsByID` before
  `reconcile`. Search and sort must use the same code-unit comparator;
  `localeCompare` has different mixed-case collation and is prohibited here.
  Server response order is not a cache-order contract.
- Escape transfers focus synchronously, then performs one hydration through the
  existing SDK and sync context. SSE remains the primary live path; this
  hydration is not a timer, poller, retry, or second client.
- Filter hydrated rows to Session ids in the current run tree. Unrelated Session
  interactions must not enter the owning Session prompt.
- If the run tree is unavailable, focus still returns to Main without an
  interaction request. A failed hydration keeps existing state and reports a
  contextual error toast.

### 4. Validation & Error Matrix

- Descendant Permission pending after observation -> Main renders the Permission
  prompt after Escape.
- Descendant Question pending after observation -> Main renders the Question
  prompt after Escape.
- Interaction belongs to another tree -> exclude it.
- Descending or otherwise unsorted bootstrap Sessions -> sort by the shared
  comparator before cache insertion; `sync.session.get(route.sessionID)` must
  remain defined.
- Mixed-case Session ids -> cache remains ordered and unique; sync/event updates
  replace the matching row instead of inserting a duplicate.
- Missing run tree -> focus Main; make no hydration request.
- Permission or Question list failure -> preserve current state and show an
  error; never leave the observation focused.

### 5. Good / Base / Bad Cases

- Good: `/tui/bootstrap` returns Sessions in descending code-unit order; the
  client normalizes them, and mixed-case updates keep one reachable row per id.
  Escape from a grandchild observation then renders its pending Permission in
  Main.
- Base: SSE already populated the interaction stores; hydration replaces them
  with the same server state.
- Bad: sort with `localeCompare` while binary-searching with `<`, install raw
  bootstrap order, infer focus from stale transcript footer text, or render a
  descendant prompt while its observation pane owns focus.

### 6. Tests Required

- Unit: Main focus dispatch precedes prompt focus and does not steal focus from
  a modal.
- PTY at 80 and 140 columns: the proxy returns bootstrap Sessions in descending
  code-unit order; mixed-case Session updates do not create unreachable cache
  rows; the root Session remains renderable; observation input stays inert;
  Escape restores the Main draft; a descendant Permission remains pending,
  renders in Main, and can be answered exactly once.
- The PTY must wait for fresh Main-focus evidence before sending the next
  semantic input. Post-Escape increments of both GET `/permission` and GET
  `/question` acknowledge the focus handler. Generic `ctrl+p commands`
  transcript text can be a stale incremental repaint and is not focus evidence.
  Time sleeps alone do not prove terminal focus ownership.

### 7. Wrong vs Correct

#### Wrong

```typescript
setStore("session", reconcile(bundle.sessions))

await writeInput(escapeKey)
await writeInput(marker)
```

Raw server order violates binary-search lookup. The Main workspace can be
focused while `sync.session.get(route.sessionID)` is undefined, which removes
the complete Main content subtree. Back-to-back input also lacks fresh evidence
that the Escape handler ran.

#### Correct

```typescript
const compareSessionIDs = (left: string, right: string) =>
  left === right ? 0 : left < right ? -1 : 1
const sortSessionsByID = (sessions: Session[]) =>
  sessions.toSorted((a, b) => compareSessionIDs(a.id, b.id))

setStore("session", reconcile(sortSessionsByID(bundle.sessions)))

focusMainPromptOwnership({ dispatch, prompt, modalActive })
void refreshTreeInteractions()

const permissionBefore = getRequestCount("/permission")
const questionBefore = getRequestCount("/question")
await writeInput(escapeKey)
await waitFor(
  () =>
    getRequestCount("/permission") > permissionBefore &&
    getRequestCount("/question") > questionBefore,
  "Main focus hydration",
)
await writeInput(marker)
```

One code-unit comparator protects sorting, lookup, and insertion from mixed-case
collation drift. Fresh request counts prove the actual Escape handler ran before
the PTY sends semantic input.

---

## Scenario: Fail-Closed Model Catalog Presentation

### 1. Scope / Trigger

- Trigger: changing TUI bootstrap/sync, model selection, Recent/Favorite state,
  provider status, or prompt admission.

### 2. Signatures

- Decode through `decodeCatalogProviders` and `decodeCatalogSelection` in
  `src/hya/model-catalog.ts`; use existing SDK sync contexts for transport.

### 3. Contracts

- Components consume only exact backend snapshot rows and typed
  `source`/`auth`/`result` metadata. No frontend catalog cache or HTTP client.
- Persisted recents, favorites, variants, agent defaults, and Session models stay
  stored but remain hidden when their exact provider/model row is absent.
- Offline is selectable only when the backend supplies `hya/offline`.

### 4. Validation & Error Matrix

- Unknown metadata -> fail closed (`none`, `unavailable`, or no row).
- Missing selected row -> use the row-backed backend default, then another real
  row; if none exists, stop prompt submission with a clear warning.

### 5. Good/Base/Bad Cases

- Good: anonymous configured rows are selectable without a health claim.
- Base: backend offline row permits local echo prompts.
- Bad: provider-array length, a stale Recent row, or Session metadata creates a
  selection.

### 6. Tests Required

- Bun tests cover all typed statuses, malformed input, exact stale-state
  filtering, backend default membership, offline-only, and live-no-offline.
- Actual TUI smoke covers selector labels and offline prompt output.

### 7. Wrong vs Correct

#### Wrong

```typescript
const connected = providers.length > 0
const selected = recent[0]
```

#### Correct

```typescript
const providers = decodeCatalogProviders(sync.data.provider)
const selected = filterCatalogSelections(providers, recent)[0]
```
---

## Scenario: Coding-Tool SDK-to-OpenTUI Quality Gate

### 1. Scope / Trigger

- Trigger: changing the hya-owned coding-tool normalizer/renderer, the retained
  Session route dispatch, SyncProvider part replacement, or OpenTUI width,
  syntax, diff, fallback, and replay behavior.
- Applies to the one-way boundary `Tool execution -> committed ToolResult or
  ToolError -> Projection/Compat SDK ToolPart -> hya presentation`. Backend
  schemas, permission ordering, typed hashline errors, bounded state, and
  commit-boundary reconciliation are owned by the backend; this scenario proves
  that the frontend does not reinterpret or bypass them.
- The canonical model names are `read`, `edit`, `grep`, `write`, and `bash`.
  `shell` is a hidden runtime alias only. Task's empty nested description is
  normalized by the backend and is not a frontend compatibility field.

### 2. Signatures

- `presentCodingTool(part: ToolPart): CodingToolView | undefined` is the single
  `unknown`/metadata validation seam. It accepts projected SDK parts and emits
  an allowlisted view union or `undefined` for the existing fallback.
- `CodingToolPresentation(props)` renders the normalized `CodingToolView` passed
  by the Session route. Isolated callers may pass one projected `ToolPart`.
  It may own collapse state, but no synchronized server state.
- The projected input is `ToolPart { type: "tool", tool, callID, state }`;
  completed `state` carries the backend result envelope `{ title, output,
  metadata }`. The normalizer reads only documented fields:
  `metadata.display` file facts `{type,path,text,lineStart,lineEnd,totalLines,
  truncated}` or Grep `groups[]` with `{path,rows[]}`, where each row has
  `{line,text,isMatch}`. It combines top-level `truncated` with every backend
  result-cap truncation flag rather than trusting one adapter-specific field.
- The backend schema owner publishes Read `{path,offset?,limit?,raw?}`, Edit
  `{path,edits}`, Grep `{pattern,path?,glob?,ignoreCase?,literal?,context?,limit?}`,
  Write `{path,content}`, and Bash `{command,env?,timeout?,cwd?,pty?}`. The
  frontend uses the canonical tool name and result metadata; it must not create
  a second schema or accept hidden keys as display inputs.
- SyncProvider's initial hydration and `message.part.updated` replacement are
  the only source updates. The retained Session route invokes the normalizer
  once, passes its view to the renderer, and otherwise uses its existing
  inline/generic/error path. No presentation provider owns a second message
  store, request, or timer.

### 3. Contracts

- Only a committed projected SDK part is eligible for the completed coding-tool
  block. A backend post-commit diagnostic such as `File changed at <path>` and
  a stable hashline `[E_*]` failure remain visible as error text; the frontend
  never turns either into a local success or mutates the Event history.
- Read/Write render a title, file-derived grammar, stable line numbers, and the
  backend `lineStart` offset. Edit uses the existing semantic diff primitive,
  keeps removed/added unified rows distinct at 80 columns, and treats the
  backend's final post-formatter diff/preview as authoritative. Completed
  Edit/Write diagnostics strictly validate every bounded entry, retain only the
  first three severity-one entries with `range.start`, and display them as
  one-based `Error [line:column]` labels. Grep uses
  bounded per-file groups in backend order, derives grammar per path, and marks
  match rows separately from context. Bash and hidden `shell` share one block:
  nullable exit remains valid for timeout/signal results, command highlighting
  never styles the output plane, ANSI is stripped from output, and `env` never
  renders.
- Backend truncation/artifact metadata is distinct from local collapse. The
  renderer retains top-level `truncated` and each `output`, `title`, `display`,
  `diff`, `attachments`, `diagnostics`, `warnings`, `rows`, `groups`,
  `metadata`, and `envelope` truncation flag plus `unknownFieldsDropped`. It
  may expand or collapse only the bounded payload it received and must retain
  explicit truncation or artifact status. It cannot request more bytes, read
  the target file, or treat collapsed output as a new result.
- The normalizer allowlists all fields and bounds display rows/text. Unknown
  tool names, unknown keys, malformed metadata, unsupported directory or
  attachment shapes, pending/streaming/permission/denied states, and malformed
  Task error payloads return `undefined` or use the existing readable fallback.
  Syntax parser failure keeps the title/content and uses plain text.
- Semantic status is textual as well as tonal: completed, truncated, warning,
  denied, error, timeout, and cancellation remain understandable without color.
  Use existing theme roles and width/path/code/diff primitives; no raw color,
  decorative card, Rust renderer, second SDK client, polling, timer, or Event
  replay is allowed.
- 80-column output keeps the prompt and transcript readable and uses a unified
  Edit diff. At 140 columns and wider, titles and metadata remain visible and
  Edit may split only under the existing wide-layout rule. Every file/group
  wraps or clips within the available width without ANSI or line-number drift.
- Reopening a Session must render the same completed semantic view from the
  projected SDK part. Initial hydration and one live `message.part.updated`
  replacement mutate the SyncProvider-owned part exactly once; the renderer
  does not preserve a parallel result cache, make a presentation request, or
  schedule a presentation timer. This is the frontend side of commit-boundary
  reconciliation.

### 4. Validation & Error Matrix

| Input/state | Required frontend result |
| --- | --- |
| Valid completed Read/Write metadata | File title, grammar spans, stable numbers, correct `lineStart`, bounded local collapse |
| Valid completed Edit metadata | Existing semantic diff; unified at 80 columns, wide split only when allowed |
| Valid completed Grep groups | One titled block per file, backend order, per-file grammar, visible match/context distinction |
| Valid completed Bash or hidden `shell` | One command/output block, command-only highlighting, ANSI-free plain output, no `env` |
| Valid Bash with nullable exit and timeout/signal metadata | Preserve output and show the textual termination state; do not drop the specialized view |
| Completed Edit/Write diagnostics | First three severity-one positioned entries, labeled `Error [line:column]` with one-based coordinates |
| Adapter-specific truncation without top-level truncation | View remains explicitly truncated; local collapse cannot erase the backend fact |
| Stable hashline error (`[E_BAD_REF]`, `[E_STALE_ANCHOR]`, `[E_EDIT_CONFLICT]`, etc.) or post-commit `File changed at` error | Preserve bounded error text and fallback status; do not classify as success or rewrite it |
| Pending/running/permission/denied/cancelled/timeout | Existing lifecycle/error presentation with text status; do not synthesize a completed block |
| Directory, image/PDF attachment, or unsupported result shape | Existing specialized attachment/directory or generic fallback; no forced file renderer |
| Unknown key/tool, malformed `metadata.display`, wrong row/path/line type | `presentCodingTool` returns `undefined`; generic/error fallback only |
| Syntax grammar/parser unavailable | Keep readable plain text and title; no component throw or network retry |
| Backend truncation/artifact flag | Show explicit bounded/truncated/artifact status; collapse cannot hide that fact |
| `env` or unknown input fields | Never render their values, even when output is otherwise valid |
| 80 columns | Prompt remains usable, blocks wrap/clip safely, Edit is unified |
| 140+ columns | Wide metadata/title remains visible, Edit follows wide diff rule, no independent reflow semantics |
| Session reopen or one SDK part replacement | Same completed semantic output; no presentation HTTP call, timer, or Event replay |

### 5. Good / Base / Bad Cases

- Good: SDK hydration supplies a completed Read with `lineStart: 41`; the block
  displays line 41, Rust syntax spans, and a bounded title. A later part update
  replaces its output, and reopening the Session produces the same final block.
- Good: an Edit's formatter changes the final bytes; the projected diff and
  metadata render those final bytes at 80 and 140 columns instead of replaying
  the pre-formatter request input.
- Base: a hidden `shell` part renders as Bash, a stable `[E_STALE_ANCHOR]`
  remains a readable error, malformed display metadata uses generic fallback,
  and syntax failure uses plain text.
- Bad: reading `part.state.input.path` to fetch or reconstruct content, sorting
  Grep groups locally, exposing `env`, treating backend truncation as collapse,
  or using a full-screen snapshot that passes only at one terminal width.
- Bad: a second component decoder accepts arbitrary metadata, a completed
  frontend result is rendered before the SDK projection commit, or a malformed
  Task description is silently presented as a successful spawn.

### 6. Tests Required

- `test/coding-tool-presentation.test.ts` starts with failing cases against the
  old inline/generic path and asserts canonical/hidden tool-name normalization,
  allowlisted fields, stable hashline error preservation, final-state/error
  fallback, every independent backend truncation fact, strict malformed
  diagnostics, directory/attachment handling, ANSI stripping, and
  `env`/unknown-key exclusion.
- `test/coding-tool-render.test.tsx` uses OpenTUI `testRender` at 80 and 140
  columns. Assert titles, syntax-colored `captureSpans()`, Read offsets and
  line numbers, Edit unified/split mode, Grep per-file order and match labels,
  Bash command-only highlighting, text status labels, and no horizontal
  corruption. Avoid brittle full-screen snapshots.
- `test/coding-tool-sync.test.tsx` hydrates one completed SDK part, applies one
  `message.part.updated` replacement, and asserts one rendered replacement with
  no presentation-specific network call, timer, polling, Event replay, or
  second store. Reopened-Session equivalence belongs to the real PTY scenario.
- Focused backend/SDK fixture tests consumed by the frontend must assert the
  exact canonical schemas, typed hashline failures, Task empty/non-empty
  description behavior, result caps, and final post-formatter metadata; the
  frontend tests must not recreate those backend rules with ad hoc casts.
- A real backend plus hya-ts PTY test produces Read, Edit, Write, Grep, and Bash
  results at 140 and 80 columns, reopens the Session, and compares semantic
  completed blocks. Assert prompt/footer readability, unified narrow diff,
  syntax spans, replay equality, and absence of secrets.
- Mutation proof should fail if the renderer fetches, polls, replays Events,
  accepts an unallowlisted key, drops `lineStart`, changes Grep order, renders
  ANSI/output as highlighted code, or removes the malformed-data fallback.

### 7. Wrong vs Correct

#### Wrong

```tsx
createEffect(async () => {
  const response = await fetch(`/tool/${props.part.id}`)
  setLocalResult(await response.json())
})

return <text>{String(props.part.state.output)}</text>
```

This bypasses the SDK projection/commit boundary, creates a second state owner,
loses replay determinism, and renders unvalidated data and possible secrets.

#### Correct

```tsx
const view = createMemo(() => presentCodingTool(props.part))

return view() ? (
  <CodingToolPresentation
    view={view()!}
    width={ctx.width}
    diffStyle={ctx.tui.diff_style}
    diffWrapMode={ctx.diffWrapMode()}
  />
) : (
  <GenericTool {...toolprops} />
)
```

The route consumes one projected part, normalizes it once, and passes that view
to the renderer. SyncProvider supplies live and replayed state, while malformed
or unsupported data stays in the existing fallback.
