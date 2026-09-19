# hya-tui-ts tests

19 Bun test files under this directory. Run from the package root:

```sh
cd packages/hya-tui-ts
bun test
```

OpenTUI Solid is preloaded via `bunfig.toml` (`[test] preload`).

> The retired Track T suites (`real-backend.test.ts`,
> `real-backend-agents.test.ts`, `pty-smoke.test.ts`,
> `workflow-pty.test.ts`, `sdk-spine.test.ts`,
> `agent-model-sync.test.tsx`, `coding-tool-sync.test.tsx`) verified this
> frontend against the deleted legacy HTTP surface. They were removed together
> with that surface; this package is expected to break at runtime against the
> consolidated `/v1` contract until the new TUI lands.

## Which suites need `hya-backend`

| Suite | Needs `target/debug/hya-backend` |
| --- | --- |
| `workflow-presentation.test.ts` | No — deterministic projection unit coverage |
| `workflow-sidebar.test.ts` | No — static host/sidebar integration seam |
| `startup-trace.test.ts` | No |
| `agent-visibility.test.ts` | No |
| `task-presentation.test.ts` | No (unit) |
| `subagent-workspace.test.ts` | No (unit) |
| `boundary.test.ts` | No |
| `branding-pruning.test.ts` | No |
| `runtime-boundary.test.ts` | No (uses `bun install` + prune; no Rust backend) |

## Architecture / invariant guards

Three suites are **architecture guards**. Their failures mean a boundary was
violated, not that a feature test regressed.

### `boundary.test.ts`

Enforces the **pinned legal and source boundary**:

- `LICENSE` matches the upstream OpenCode MIT text
- `UPSTREAM.md` still records provenance (repo, version, commit, `packages/tui`,
  Imported/Excluded boundary headings)
- `package.json` dependency pins stay exact (SDK/plugin/OpenTUI versions, etc.)
- Source tree stays within allowed relative paths and extensions
- Imports must not pull forbidden modules; third-party deps must be in the pin map

**Failure means:** dependency drift, missing provenance, or an import outside the
frontend-only boundary.

### `branding-pruning.test.ts`

Enforces **hya presentation and pruning**:

- `auditSurface` product/theme/path/command constants stay on `hya` branding
- Static builtin plugin ids remain the expected internal list
- Reachable source must not reintroduce excluded OpenCode console/share/workspace
  adapter APIs or product strings (with a small allowlist for protocol constants
  such as `x-opencode-directory`)

**Failure means:** rebrand regression or reintroduction of excluded product
surfaces.

### `runtime-boundary.test.ts`

Enforces the **prepared runtime SDK shape**:

- Copies package metadata + `src` into a temp dir
- `bun install --production`
- Runs `scripts/prune-sdk-server.ts` on that runtime
- Verifies `import { createOpencodeClient } from "@opencode-ai/sdk/v2"` works
- Builds `src/main.tsx` against the pruned tree

**Failure means:** the prune script or SDK package layout no longer yields a
client-only importable runtime (install/release packaging would break).

## Other suites (short)

| Suite | Role |
| --- | --- |
| `agent-visibility.test.ts` | Which agents appear in TUI selector vs subagent autocomplete |
| `agent-models.test.ts` | Agent model row decoding, capability gating, and `/agent-models` command map |
| `model-catalog.test.ts` | Model catalog decoding and picker rows |
| `coding-tool-presentation.test.ts` | Coding-tool view mapping from projected SDK parts |
| `coding-tool-render.test.tsx` | Narrow/wide coding-tool layout |
| `tool-card.test.tsx` | Tool-call card title formatting and rendered frame/padding |
| `context-status.test.ts` | Session context-occupancy decode and presentation (token accounting surface) |
| `heap-snapshot.test.ts` | Palette heap-snapshot writer path, permissions, and errors |
| `keybind-inventory.test.ts` | Shipped keybind registry matches current command docs |
| `task-presentation.test.ts` | Multi-member task presentation helpers (unit) |
| `subagent-workspace.test.ts` | Run-tree / split-pane workspace reducer (unit) |
| `queued-prompts.test.ts` | Queued-prompt admission helpers (unit) |
| `startup-trace.test.ts` | `HYA_STARTUP_TRACE` JSON mark emission |
| `workflow-presentation.test.ts` | Workflow projection parsing and deterministic status/progress text |
| `workflow-sidebar.test.ts` | First registration, session-state synchronization, and sidebar rendering |
