# hya-tui-ts tests

Eleven Bun test files under this directory. Run from the package root:

```sh
cd packages/hya-tui-ts
bun test
```

OpenTUI Solid is preloaded via `bunfig.toml` (`[test] preload`).

## Track T scope

**Track T** exercises the real frontend ↔ backend path over the pinned
`@opencode-ai/sdk/v2` client: session/prompt, permissions, questions, multi-agent
roster, and (where marked) a full PTY session through `hya-ts`.

These suites assume a built debug backend:

```sh
cargo build -p hya-backend --bin hya-backend
```

PTY smoke also needs the launcher:

```sh
cargo build -p hya-ts --bin hya-ts
```

Focused Track T files (also listed in root `AGENTS.md`):

```sh
bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

## Which suites need `hya-backend`

| Suite | Needs `target/debug/hya-backend` |
| --- | --- |
| `real-backend.test.ts` | **Yes** — spawns `serve` on an ephemeral port |
| `real-backend-agents.test.ts` | **Yes** — multi-agent roster via live API |
| `pty-smoke.test.ts` | **Yes** (plus `hya-ts`) — PTY cases drive a real backend and launcher |
| `sdk-spine.test.ts` | No — local `Bun.serve` mock for HTTP/SSE |
| `startup-trace.test.ts` | No |
| `agent-visibility.test.ts` | No |
| `task-presentation.test.ts` | No (unit) |
| `subagent-workspace.test.ts` | No (unit) |
| `boundary.test.ts` | No |
| `branding-pruning.test.ts` | No |
| `runtime-boundary.test.ts` | No (uses `bun install` + prune; no Rust backend) |

If the backend binary is missing, Track T / PTY tests fail at spawn with a
filesystem or process error — not a soft skip.

## Which suites spawn a PTY

| Suite | PTY |
| --- | --- |
| `pty-smoke.test.ts` | **Yes** — Linux PTY sessions via `hya-ts`, terminal restore, observation panes |
| All others | No |

PTY cases are Linux-oriented (stty / process-group checks). Expect failures or
environment skips on platforms without that shell setup.

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
| `task-presentation.test.ts` | Multi-member task presentation helpers (unit) |
| `subagent-workspace.test.ts` | Run-tree / split-pane workspace reducer (unit) |
| `sdk-spine.test.ts` | `launch` + `observeSdkSpine` against a mock server |
| `startup-trace.test.ts` | `HYA_STARTUP_TRACE` JSON mark emission |
| `real-backend*.test.ts` | Track T live backend contracts |
| `pty-smoke.test.ts` | Track T PTY end-to-end smoke |
