# hya

hya is an event-sourced, terminal-first multi-agent coding agent written in
Rust. It runs an event-sourced session engine, normalizes model providers
(OpenAI-compatible, Anthropic, and Google) into one canonical event stream,
executes tools behind a permission plane, and exposes the same core through an
interactive TUI, headless CLI commands, and an HTTP/SSE server with an
OpenCode-compatible surface.

If no provider is configured, hya still runs: it falls back to an offline
"dev" provider that echoes prompts, so the whole stack is usable without API
keys while you set things up.

## Status

hya is under active development (workspace version `0.28.9`,
`MIT OR Apache-2.0`). It is not yet published as a prebuilt binary or to
crates.io — build it from source as described below. APIs, config, and command
surface may still change between versions.

## Build From Source

Requires a Rust toolchain matching the workspace manifest
([`Cargo.toml`](Cargo.toml); currently edition 2024, Rust `1.91`).

```sh
git clone <this-repo> hya
cd hya
cargo build --workspace
```

The shipped frontend binary is `hya` (crate [`crates/hya`](crates/hya)); the
backend CLI/API binary is `hya-backend` (crate [`crates/hya-backend`](crates/hya-backend)).
Run them directly from the workspace:

```sh
cargo run -p hya --                    # interactive frontend
cargo run -p hya-backend -- exec "summarize this repository"
```

Or install it onto your `PATH`:

```sh
cargo install --path crates/hya
hya                                # interactive TUI
```

## Configure a Provider and Log In

By default `hya` starts offline. To use a live model, create
`~/.config/hya/config.yaml` (or `$XDG_CONFIG_HOME/hya/config.yaml`):

```yaml
default_model: claude-sonnet-4-6
providers:
  anthropic:
    kind: anthropic
    base_url: https://api.anthropic.com/v1
    api_key: "{env:ANTHROPIC_API_KEY}"
    models: [claude-sonnet-4-6]
```

You can supply the key inline (via `{env:VAR}`, `{file:/path}`, or a literal)
or store it with `hya-backend login`, which takes precedence over an inline `api_key`:

```sh
hya-backend login anthropic "$ANTHROPIC_API_KEY"
hya-backend models  # inspect the resolved catalog
hya                 # start the TUI against the live provider
```

See [docs/configuration.md](docs/configuration.md) for first-run behavior,
the full environment-variable reference, and a fully-commented sample config.

## What hya Can Do

- Interactive TUI with slash commands, model/agent selection, permission
  prompts, transcript export, and session resume.
- Headless single-turn execution (`hya-backend exec` / `hya-backend run`) and iterative goal
  mode (`hya-backend -p "<goal>"`).
- HTTP/SSE server (`hya-backend serve`) exposing native `/sessions/*` routes plus
  OpenCode-compatible route groups.
- MCP servers, plugins (including an OpenCode plugin adapter), and a formatter
  plane, all driven from the same config.

## Documentation

| Page | Purpose |
| --- | --- |
| [docs/README.md](docs/README.md) | Documentation index and reading paths. |
| [docs/getting-started.md](docs/getting-started.md) | Zero-to-running: build, run the TUI, headless turns, goal mode, server, and a first live provider. |
| [docs/configuration.md](docs/configuration.md) | Config file, first-run/offline behavior, `HYA_*` env vars, providers/auth, MCP, plugins, formatter, custom commands. |
| [docs/cli.md](docs/cli.md) | `hya` commands, flags, and the TUI slash-command reference. |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Common local, provider, terminal, permission, and server issues. |
| [docs/project-structure.md](docs/project-structure.md) | Repository layout, crates, and data flow. |
| [docs/architecture/](docs/architecture) | Engine, event model, providers, tools/permissions, storage, server/client, and TUI internals. |
| [docs/opencode-parity.md](docs/opencode-parity.md) | OpenCode compatibility status. |
| [docs/hya-pi-opencode-comparison.md](docs/hya-pi-opencode-comparison.md) | Feature comparison across hya, upstream stock Pi, and current OpenCode. |

## License

Licensed under either of MIT or Apache-2.0 at your option.
