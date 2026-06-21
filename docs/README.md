# yaca Documentation

yaca is a Rust workspace for a terminal-first coding agent. It runs an
event-sourced session engine, normalizes model providers into one event stream,
executes tools behind a permission plane, and exposes the same core through an
interactive TUI, headless CLI commands, and an HTTP/SSE server.

This documentation is split into user-facing guides and maintainer-facing
architecture notes.

## Reading Paths

If you want to run yaca:

1. [Getting Started](getting-started.md)
2. [Configuration](configuration.md)
3. [CLI Reference](cli.md)
4. [Troubleshooting](troubleshooting.md)

If you want to understand the codebase:

1. [Project Structure](project-structure.md)
2. [Architecture Overview](architecture/overview.md)
3. [Runtime](architecture/runtime.md)
4. [Event Model](architecture/event-model.md)
5. [Providers](architecture/providers.md)
6. [Tools and Permissions](architecture/tools-and-permissions.md)
7. [Storage](architecture/storage.md)
8. [Server and Client](architecture/server-client.md)
9. [TUI](architecture/tui.md)
10. [Development](development.md)

## Docs Map

| Page | Purpose |
| --- | --- |
| [Getting Started](getting-started.md) | Build and run the TUI, a headless prompt, a goal run, and the server. |
| [Configuration](configuration.md) | Explain model/provider discovery through opencode config and environment overrides. |
| [CLI Reference](cli.md) | Document shipped `yaca` commands and flags. |
| [Project Structure](project-structure.md) | Map repository paths, crates, modules, tests, and data flow. |
| [Architecture Overview](architecture/overview.md) | Explain the crate boundary model and end-to-end request path. |
| [Runtime](architecture/runtime.md) | Explain `SessionEngine`, turn execution, goal mode, loop mode, teams, and worktrees. |
| [Event Model](architecture/event-model.md) | Explain canonical events, envelopes, messages, parts, ids, and projections. |
| [Providers](architecture/providers.md) | Explain provider routing, OpenAI-compatible and Anthropic protocols, SSE decoding, and fallback providers. |
| [Tools and Permissions](architecture/tools-and-permissions.md) | Explain builtin tools, permission rules, ask flows, and output limits. |
| [Storage](architecture/storage.md) | Explain SQLite persistence, replay, projections, and token ledger behavior. |
| [Server and Client](architecture/server-client.md) | Explain the HTTP API, SSE stream, and typed client crate. |
| [TUI](architecture/tui.md) | Explain the split between terminal event loop and pure ratatui rendering. |
| [Development](development.md) | Explain build, lint, test, crate-change, and doc-update workflow. |
| [Troubleshooting](troubleshooting.md) | Collect common local, provider, terminal, permission, and server issues. |

## Source Entrypoints

- Workspace manifest: [`../Cargo.toml`](../Cargo.toml)
- CLI binary: [`../crates/yaca-cli/src/main.rs`](../crates/yaca-cli/src/main.rs)
- Core engine: [`../crates/yaca-core/src/engine.rs`](../crates/yaca-core/src/engine.rs)
- Protocol types: [`../crates/yaca-proto/src/lib.rs`](../crates/yaca-proto/src/lib.rs)
- Providers: [`../crates/yaca-provider/src/lib.rs`](../crates/yaca-provider/src/lib.rs)
- Tools: [`../crates/yaca-tool/src/lib.rs`](../crates/yaca-tool/src/lib.rs)
- Store: [`../crates/yaca-store/src/lib.rs`](../crates/yaca-store/src/lib.rs)
- Server: [`../crates/yaca-server/src/lib.rs`](../crates/yaca-server/src/lib.rs)
- TUI renderer: [`../crates/yaca-tui/src/lib.rs`](../crates/yaca-tui/src/lib.rs)
