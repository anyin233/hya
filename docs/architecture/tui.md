# TUI Architecture

The shipped interactive frontend is the TypeScript/OpenTUI application under
[`../../packages/hya-tui-ts`](../../packages/hya-tui-ts). The Rust `hya`
binary is only its canonical Unix entrypoint.

## Process Chain

```text
hya
  -> exec adjacent hya-ts
  -> start or attach to hya-backend
  -> run packages/hya-tui-ts/src/main.tsx with Bun
  -> use @opencode-ai/sdk/v2 over HTTP/SSE
```

[`../../crates/hya/src/main.rs`](../../crates/hya/src/main.rs) resolves
`hya-ts` beside the current executable and replaces its own process with it.
There is no PATH lookup or Rust frontend fallback. `arg0` remains `hya`, so
canonical help and errors use the public product name.

[`../../crates/hya-ts`](../../crates/hya-ts) owns launcher argument parsing,
runtime and backend discovery, terminal process-group handoff, signal cleanup,
and terminal restoration. It either:

- starts an owned local `hya-backend` through `hya-sdk`; or
- attaches to the URL supplied by `--server`.

The launcher then runs Bun from the prepared runtime directory. Installed and
release layouts place it at `../lib/hya/hya-tui-ts` relative to the binaries.
`HYA_TUI_TS_DIR`, `HYA_BACKEND_BIN`, `--backend-bin`, and `--bun` provide
explicit development or diagnostic overrides.

## Frontend Ownership

The TypeScript package owns terminal rendering and interaction:

- SolidJS/OpenTUI application state and routes
- prompt, transcript, dialogs, themes, keybindings, and command palette
- session, model, agent, MCP, permission, and question views
- SDK HTTP calls and SSE synchronization

The package is frontend-only. Provider execution, tools, permissions, events,
and persistence remain in `hya-backend` and its Rust library dependencies. The
frontend consumes the Compat-shaped SDK surface instead of constructing a
second runtime or projection.

## Sessions and Startup

The public frontend accepts `--continue`, `--session <ID>`, and `--fork`.
`--fork` requires either `--continue` or `--session`. Bare
`hya-backend --resume <ID>` remains supported and launches
`hya --session <ID>`.

The removed Rust frontend options `--db`, `--yolo`, `--http`, `--compat`, and
`--resume` are not part of the public `hya` launcher. Use backend configuration,
the TUI command palette, `--server`, and frontend `--session` respectively.

## Retained Rust UI Crates

[`../../crates/hya-tui`](../../crates/hya-tui) and
[`../../crates/hya-tui-lib`](../../crates/hya-tui-lib) remain in the workspace
for compatibility and existing tests, but no shipped binary launches their
renderer or controller. New interactive behavior belongs in
`packages/hya-tui-ts`; reusable protocol behavior belongs below the HTTP/SDK
boundary.

`hya-backend` may launch the current `hya` frontend for bare interactive
startup, but it does not own a terminal renderer.

## Installation Contract

The frontend is a colocated installation, not a standalone Cargo binary. The
supported installer and release archives contain:

```text
bin/hya
bin/hya-ts
bin/hya-backend
lib/hya/hya-tui-ts/
```

Bun must be available when installing and running hya. The installer prepares
production dependencies with the pinned lockfile and removes SDK server code
that the frontend does not use.
