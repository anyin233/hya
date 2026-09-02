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
