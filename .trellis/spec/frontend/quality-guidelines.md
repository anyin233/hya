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

- Reconnecting a shipped binary to the retained Rust TUI renderer.
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

- Does the change live in `packages/hya-tui-ts`, not the retained Rust UI?
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
