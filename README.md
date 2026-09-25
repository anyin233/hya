
<p align="center">
  <img src="docs/assets/hya-icon-8bit.png" alt="Hya icon" width="20%"><br>
</p>

---

hya is an event-sourced, multi-agent coding agent. `hya` owns the
runtime: it normalizes OpenAI Chat/Responses/Codex, Grok Build, Anthropic, and
Google provider routes into one canonical event stream and executes tools behind
a permission plane. Every client-facing surface speaks one consolidated contract
— `hya.v1` — served identically over HTTP/JSON+SSE+WebSocket (`/v1`) and gRPC
(`HYA_GRPC_BIND`); the legacy Compat and native HTTP routes are gone. The
interactive frontend is the Bun/OpenTUI TUI in `packages/hya-tui`, run from
source; one command starts it together with its own `hya serve` (see
[Run the TUI](#run-the-tui)). The same TUI renders in a browser as the WebUI
through `packages/hya-tui-web`. Other clients use `hya-sdk-v1`, `hya-client`,
or any `hya.v1` client.


If no provider is configured, hya still runs: it falls back to an offline
"dev" provider that echoes prompts, so the whole stack is usable without API
keys while you set things up.

## Status

hya is under active development (workspace version `0.41.0`,
`MIT OR Apache-2.0`). The latest public binary release is `v0.35.1`; the
checked-out `0.41.0` workspace is newer and is not published to crates.io. Build
this checkout from source as described below. APIs, config, and command surfaces
may still change between versions.


## Build From Source

Requires a Rust toolchain matching the workspace manifest
([`Cargo.toml`](Cargo.toml); currently edition 2024, Rust `1.91`), Git, and Bun
(pinned at 1.4.2 for the Bun adapter runtime).

```sh
git clone <this-repo> hya
cd hya
./install.sh --prefix "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
hya serve
```

The installer places `bin/hya`, the twelve first-party bundles it loads
at startup under `bundles/`, and `lib/hya/bun-adapter/` with its production
dependencies. Release archives use the same layout; each first-party bundle is
also published as a standalone release asset (see
[first-party bundles](docs/bundle-runtime.md#release-assets)).


## Run the TUI

From the checkout, with `hya` on `PATH` (or `HYA_BIN` pointing at a build):

```sh
(cd packages/hya-tui && bun install --frozen-lockfile)
bun packages/hya-tui/src/main.ts --dir "$PWD"            # starts and stops its own hya serve
bun packages/hya-tui/src/main.ts --dir "$PWD" --continue # reopen the last session
```

`?` lists every key and command. Pass `--server <url>` to use a backend you
run yourself instead. See the [TUI guide](docs/tui.md#start-it).

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
or store it with `hya login`, which takes precedence over an inline `api_key`:

```sh
hya login anthropic "$ANTHROPIC_API_KEY"
hya models  # inspect the resolved catalog
hya serve   # start the HTTP/SSE server against the live provider
```

For ChatGPT Codex or Grok Build subscription OAuth (no API key):

```sh
hya oauth login --provider codex --type openai-codex
hya oauth login --provider grok --type grok-build
hya oauth status
```

See [docs/configuration.md](docs/configuration.md) for first-run behavior,
the `HYA_*` (and related) environment-variable tables, and a fully-commented
sample config.

## What hya Can Do

- Headless single-turn execution (`hya exec` / `hya run`) and iterative goal
  mode (`hya -p "<goal>"`).
- HTTP/SSE/WebSocket server (`hya serve`) exposing the consolidated
  `hya.v1` contract under `/v1`, plus optional gRPC via `HYA_GRPC_BIND`; see the
  [protocol guide](docs/protocol/README.md) and generated
  [API reference](docs/protocol/api-reference.md). Typed clients:
  `hya-sdk-v1` and `hya-client` (crates), or any generated `hya.v1` stub.
- MCP servers, plugins (including a Compat plugin adapter), and a formatter
  plane, all driven from the same config.

Public AgentBundles may remain static/process-free or supply selected
Bundle-local Bun sidecar capabilities. Public WorkflowBundles package one
compiled Workflow with its exact reachable Agent closure. Both kinds can be
inspected and installed with `hya bundle info -f example.hyabundle` and
`hya bundle install example.hyabundle`. See the
[AgentBundle authoring guide](docs/agent-bundle-authoring.md),
[Workflow and WorkflowBundle guide](docs/workflows.md),
[static example](docs/examples/bundle.hya.md),
[transient Bun example](docs/examples/bun-transient/),
[resident Bun example](docs/examples/bun-resident/),
[disjoint Bun example](docs/examples/bun-disjoint/), and the full
[Argus WorkflowBundle example](bundles/examples/argus-example/) plus the
[CLI reference](docs/cli.md#bundle-commands).

## Documentation

| Page | Purpose |
| --- | --- |
| [docs/README.md](docs/README.md) | Documentation index and reading paths. |
| [docs/getting-started.md](docs/getting-started.md) | Zero-to-running: build, headless turns, goal mode, server, and a first live provider. |
| [docs/configuration.md](docs/configuration.md) | Config file, first-run/offline behavior, `HYA_*` env vars, providers/auth, MCP, plugins, formatter, custom commands. |
| [docs/tui.md](docs/tui.md) | OpenTUI frontend setup, commands, and v1 interface contracts. |
| [docs/tui-web.md](docs/tui-web.md) | Browser-rendered TUI (WebUI host) and the Playwright TUI test harness. |
| [docs/cli.md](docs/cli.md) | `hya` commands, flags, and exit codes. |
| [docs/workflows.md](docs/workflows.md) | Workflow document format, governance, CLI/tool execution, and WorkflowBundle packaging. |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Common local, provider, permission, and server issues. |
| [docs/project-structure.md](docs/project-structure.md) | Repository layout, crates, and data flow. |
| [docs/architecture/](docs/architecture) | Engine, event model, providers, tools/permissions, storage, and server/client internals. |
| [docs/compat-parity.md](docs/compat-parity.md) | Historical record of the pre-v1 Compat HTTP parity work (that surface is deleted; CLI aliases and the Compat plugin adapter remain). |
| [docs/hya-pi-compat-comparison.md](docs/hya-pi-compat-comparison.md) | Feature comparison across hya, upstream stock Pi, and current Compat. |

The Rust workspace is licensed under either MIT or Apache-2.0 at your option.
The checked-out Bun adapter has no separate license file; consult the
repository license files for the complete applicable terms.
