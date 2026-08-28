# hya-tui-ts

SolidJS / OpenTUI **frontend-only** package for hya. It does not start a backend,
providers, or MCP managers. It speaks the pinned `@opencode-ai/sdk/v2` HTTP/SSE
protocol to a running server.

**This package is unlaunchable without a running backend.** The entrypoint
(`src/main.tsx`) **requires** `--url <backend-base-url>`. If you omit it, launch
throws `--url is required`. Normal users should run `hya` / `hya-ts`, which start
or attach to `hya-backend` and pass `--url` for you.

| Deeper docs | Topic |
| --- | --- |
| [UPSTREAM.md](./UPSTREAM.md) | OpenCode provenance, import boundary, rebrand notes |
| [docs/architecture/tui.md](../../docs/architecture/tui.md) | Process chain, launcher handoff, package boundaries |
| [docs/tui-reference.md](../../docs/tui-reference.md) | Screens, transcript, dialogs, prompt |
| [docs/tui-keybindings.md](../../docs/tui-keybindings.md) | Keybinds and slash commands |

## Requirements

- **Bun** `1.3.14` (`packageManager` in `package.json`)
- A running **`hya-backend`** (or any server that implements the pinned SDK v2
  HTTP/SSE surface) listening at the URL you pass as `--url`
- For Track T real-backend / PTY suites: a built
  `target/debug/hya-backend` (and for PTY, `target/debug/hya-ts`)

## Install

From this package directory:

```sh
cd packages/hya-tui-ts
bun install --frozen-lockfile
```

Workspace installs from the monorepo root also work when you already use the
repo’s Bun workflow.

## Commands

| Command | What it does |
| --- | --- |
| `bun run build` | Bundle `src/main.tsx` → `dist/` (`--target bun --packages external`) |
| `bun test` | Run the suite under `test/` (see [test/README.md](./test/README.md)) |
| `bun run typecheck` | `tsgo --noEmit` |

## Run

**Preferred:** use the product launcher so a backend is always provided:

```sh
# from repo root, after building Rust bins
cargo build -p hya-backend --bin hya-backend
cargo build -p hya-ts --bin hya-ts   # or hya shim
hya .
# or attach:
hya --server http://127.0.0.1:8080
```

**Direct Bun launch** (frontend only — backend must already be up):

```sh
# Terminal A
cargo build -p hya-backend --bin hya-backend
./target/debug/hya-backend serve --bind 127.0.0.1:8080 --db hya.db

# Terminal B
cd packages/hya-tui-ts
bun src/main.tsx --url http://127.0.0.1:8080 [PROJECT]
# or after build:
bun dist/main.js --url http://127.0.0.1:8080 [PROJECT]
```

### Full flag list (`src/main.tsx`)

| Flag | Type | Meaning |
| --- | --- | --- |
| `--url <URL>` | string | **Required.** Backend base URL (HTTP). |
| `--project <DIR>` | string | Project directory (default: first positional, else `cwd`). Resolved with `realpath` and becomes `process.cwd()`. |
| `--continue` | boolean | Continue the most recent root session (forwarded as launch args). |
| `--session <ID>` | string | Resume an exact session id. |
| `--fork` | boolean | Fork the continued/selected session. |
| `--prompt <TEXT>` | string | Initial prompt text. |
| `--agent <NAME>` | string | Initial agent name. |
| `--model <PROVIDER/MODEL>` | string | Initial model ref. |

Positional: optional project path (same as `--project`).

## Layout

```text
packages/hya-tui-ts/
  src/
    main.tsx           # Bun entry — parses flags, requires --url
    hya/               # hya-owned platform, branding audit, plugin host, SDK spine
    upstream/          # OpenCode-derived TUI (SolidJS/OpenTUI) + theme assets
  scripts/             # release prune + logo art generator (see scripts/README.md)
  test/                # unit, architecture guards, Track T real-backend / PTY
  dist/                # bun build output
  UPSTREAM.md          # provenance and import/exclude boundary
  bunfig.toml          # OpenTUI Solid preload for run and test
```

- **`src/hya`** — product-owned glue: paths/flags, branding constants, static
  builtin plugin host, SDK bootstrap probe, startup tracing, Workflow wire
  validation/presentation, and audit surfaces.
- **`src/upstream`** — retained frontend from OpenCode `packages/tui` (see
  [UPSTREAM.md](./UPSTREAM.md)), including `theme/assets/*.json` and
  `assets/audio/*.mp3`. The hya-owned
  `feature-plugins/sidebar/workflow.tsx` adapter is the narrow exception needed
  to register product state through the retained slot system.
- **`scripts/`** — runtime preparation and artwork generation.
- **`test/`** — architecture invariants and Track T integration tests.

## Editing rules and upstream re-sync

1. Read [UPSTREAM.md](./UPSTREAM.md) before changing `src/upstream/**`.
2. Prefer product changes under `src/hya/**` and the small entry surface
   `src/main.tsx`.
3. Keep dependency pins exact (`package.json` is guarded by
   `test/boundary.test.ts`).
4. Do not reintroduce excluded OpenCode backend/console/plugin-manager surfaces
   (guarded by `test/branding-pruning.test.ts` and boundary import checks).
5. **TSDoc / comments on vendored upstream files:** `src/upstream` is derived
   from upstream and re-synced as a tree. Do **not** add TSDoc or local-only
   comment layers that a future re-sync would discard. The hya-owned Workflow
   sidebar adapter is explicitly exempt; keep its domain logic in
   `src/hya/workflow-presentation.ts` and its upstream file limited to slot
   rendering.

### Vendored-excluded TSDoc surface (R3)

The documentation audit listed several `src/upstream` export-heavy files as
candidates for TSDoc (S2/S3). Against [UPSTREAM.md](./UPSTREAM.md), the whole
`src/upstream` tree is the imported OpenCode frontend boundary — not a
hya-owned module tree. **These files are excluded from package-local TSDoc** so
re-sync does not silently drop comments:

| Path | Notes |
| --- | --- |
| `src/upstream/routes/session/subagent-workspace.ts` | S2 candidate; under vendored tree |
| `src/upstream/routes/session/index.tsx` | S2 candidate; under vendored tree |
| `src/upstream/feature-plugins/system/diff-viewer-file-tree-utils.ts` | S3 candidate; under vendored tree |
| `src/upstream/config/keybind.ts` | S3 candidate; under vendored tree |
| `src/upstream/keymap.tsx` | S3 candidate; under vendored tree |
| `src/upstream/config/index.tsx` | S3 candidate; under vendored tree |

**S1 (hya-owned) carries TSDoc:** `src/main.tsx`, `src/hya/platform.ts`,
`src/hya/product.ts`, `src/hya/audit.ts`, `src/hya/static-host.ts`,
`src/hya/sdk-spine.tsx`, `src/hya/startup-trace.ts`, and
`src/hya/workflow-presentation.ts`.

## Environment variables

| Variable | Effect |
| --- | --- |
| `HYA_DISABLE_MOUSE` | Truthy (`1`/`true`): disable mouse. |
| `HYA_DISABLE_TERMINAL_TITLE` | Truthy: do not set terminal title. |
| `HYA_DISABLE_COPY_ON_SELECT` | Truthy: disable copy-on-select (always on for win32). |
| `HYA_SHOW_TTFD` | Truthy: show time-to-first-draw diagnostics. |
| `HYA_WAIT_THEME` | Truthy: await terminal theme mode before first paint (classic). Default is instant dark. |
| `HYA_SYNC_PLUGIN_START` | Truthy: gate shell routes on sequential builtin plugin host start. Default paints shell immediately. |
| `HYA_VERSION` | Version string (default `local`). |
| `HYA_CHANNEL` | Channel string (default `local`). |
| `HYA_STARTUP_TRACE` | Truthy: emit JSON startup marks on stderr (`startup-trace.ts`). |
| `XDG_DATA_HOME` / `XDG_CACHE_HOME` / `XDG_CONFIG_HOME` / `XDG_STATE_HOME` | Base dirs for `HyaPaths` (`…/hya` under each). |
| `HYA_TUI_TS_DIR` | (Launcher) override package location when `hya-ts` resolves the frontend. |

## Release-time scripts

Ship-time packaging copies this package into
`lib/hya/hya-tui-ts` and runs:

```sh
bun packages/hya-tui-ts/scripts/prune-sdk-server.ts <runtime-dir>
```

Callers:

- `install.sh` (prepared runtime install)
- `.github/workflows/release.yml` (release package assembly)

The prune script rewrites the pinned SDK export map so only the v2 client remains
importable. Details: [scripts/README.md](./scripts/README.md). Guarded by
`test/runtime-boundary.test.ts`.

Logo/epilogue terminal art is regenerated with
`scripts/generate-logo-art.py` (not part of the default install path).

## `bunfig.toml` preload note

```toml
preload = ["@opentui/solid/preload"]

[test]
preload = ["@opentui/solid/preload"]
```

Bun preloads `@opentui/solid/preload` for both normal runs and `bun test` so
OpenTUI’s Solid JSX runtime is available before modules load. Do not remove this
without an equivalent Solid/OpenTUI bootstrap; tests and the TUI will fail to
compile or render.
