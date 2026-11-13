# Quick Start Guide

`hya` ships as a single unified CLI binary (`hya`) paired with the interactive
Bun/OpenTUI terminal frontend (`packages/hya-tui`) and browser WebUI host
(`packages/hya-tui-web`). Out of the box — even before configuring any API keys
— `hya` runs against its built-in offline echo model (`hya/offline`), so you can
install, launch, and explore every surface immediately.

---

## 1. Install `hya`

### Option A: Install Prebuilt Releases (Recommended)

One command installs the latest `backend/<version>` and `frontend/<version>`
releases (including a bundled Bun runtime) into `$HOME/.local`:

```sh
curl -fsSL https://hya.ed-aisys.com/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"   # add to your shell rc if needed
hya --version
```

Supported prebuilt targets: glibc Linux (`x86_64`, `aarch64`) and Apple silicon
macOS (`aarch64`). On Windows, install inside WSL 2 (see
[Install on Windows with WSL](install-wsl.md)). For headless servers without the
TUI/WebUI, pass `--backend-only`:

```sh
curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --backend-only
```

Update an existing release installation at any time:

```sh
hya update                  # update both backend and frontend
hya update --backend-only   # update only bin/hya, first-party bundles, and Bun adapter
hya update --tui-only       # update only TUI and WebUI
```

See [Install and update](install.md) for `--version`, `--prefix`, `--force`, and
mirror variables.

### Option B: Build and Install From Source

Prerequisites:

- **Rust `1.91`+** (workspace edition `2024`)
- **Bun `1.4.2`+** (used by the TUI, WebUI host, and Bun extension adapter)
- **Git**

To build and atomically stage the complete runtime layout (`bin/hya`, the 12
first-party `.hyabundle` packages under `bundles/`, and `lib/hya/{bun-adapter,tui,tui-web}`):

```sh
./install.sh --prefix "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

| `./install.sh` Option | Meaning |
| --- | --- |
| `--prefix DIR` | Install into `DIR/bin`, `DIR/bundles`, and `DIR/lib/hya` (default `/usr/local`). |
| `--bin-dir DIR` | Install backend into `DIR` (must be named `bin`; loads bundles from `DIR/../bundles`). |
| `--profile release\|dev\|debug` | Cargo profile to build and stage (default `release`). |
| `--dry-run` | Print planned build/stage/verify steps without modifying disk. |

For iterative workspace development without installing to `$HOME/.local`:

```sh
(cd packages/hya-tui && bun install --frozen-lockfile)
(cd packages/hya-tui-web && bun install --frozen-lockfile)
cargo build -p hya-backend --bin hya
./target/debug/hya
```

---

## 2. Launch the Interactive TUI & WebUI

Run bare `hya` inside any project directory on a terminal:

```sh
hya                # Terminal TUI here + WebUI on http://127.0.0.1:3250
hya --port 8000    # Serve the WebUI on a custom port (0 picks a free port)
hya --resume       # Open the session picker for the current Project (archived included)
```

What happens when you run `hya`:

1. **Persistent backend daemon:** `hya` discovers or auto-starts the detached
   backend daemon owning `$XDG_STATE_HOME/hya/sessions.db` (fallback
   `~/.local/state/hya/sessions.db`).
2. **Terminal TUI + Browser WebUI:** `hya` starts the WebUI host on
   `http://127.0.0.1:3250` and opens the OpenTUI interface in your terminal.
   Every terminal and browser tab shares the same daemon, Projects, and live
   sessions.
3. **Instant reconnects:** Pressing Ctrl+C twice or running `/exit` closes the
   TUI and WebUI while leaving the daemon running (`hya serve status`,
   `hya serve stop`, `hya serve restart`).

Essential TUI commands and shortcuts:

| Input | Action |
| --- | --- |
| `?` | Show keybindings and command help |
| `/key` | Open the **Provider View** (add providers, save keys, browse/test models) |
| `/model` | Switch or pin the active Agent's model and reasoning effort (`#high`) |
| `/agent` | Inspect/select Agents (`hya-main`, `hya-plan`, bundle agents) and configure defaults |
| `/project` | Manage multi-root **Projects** or switch workspaces |
| `/new` (`/new --temp`) | Start a fresh session in the Project (or an isolated scratch directory) |
| `/sessions` | Browse, search, and resume sessions (`a` toggles archived) |
| `/subagents` | Inspect live and archived episodic subagents in split/tab panes |
| `/workflow` | Select, run, or inspect compiled Workflow DAGs |
| `Shift+Tab` | Cycle session permission modes (`manual`, `yolo`, or bundle modes) |

See [OpenTUI Frontend](tui.md) and [Browser-Rendered TUI](tui-web.md) for full
UI details.

---

## 3. Configure a Live Model Provider

Until a live provider is configured, `hya` runs offline using `hya/offline`.
You can connect a real provider in three ways:

### Guided CLI or TUI Setup (Fastest)

```sh
hya provider add      # Interactive prompt: base URL, protocol, API key -> fetches models
hya provider list     # Inspect configured providers
hya models            # Verify resolved provider/model catalog
```

(Inside the TUI, run `/key` for the interactive Provider View.)

### Declarative `~/.config/hya/config.yaml` + `hya login`

Edit `$XDG_CONFIG_HOME/hya/config.yaml` (or `~/.config/hya/config.yaml`):

```yaml
default_model: anthropic/claude-sonnet-4-6

providers:
  anthropic:
    kind: anthropic
    base_url: https://api.anthropic.com/v1
    api_key: "{env:ANTHROPIC_API_KEY}"
    models:
      - claude-sonnet-4-6

  openai:
    kind: openai-responses
    base_url: https://api.openai.com/v1
    api_key: "{env:OPENAI_API_KEY}"
    models:
      - gpt-5
```

Store credentials out-of-band (stored auth tokens take precedence over inline
`api_key`):

```sh
hya login anthropic "$ANTHROPIC_API_KEY"
hya models --refresh
```

### Subscription OAuth (ChatGPT Codex / Grok Build)

```sh
hya oauth login --provider codex --type openai-codex
hya oauth login --provider grok --type grok-build
hya oauth status
```

 If a daemon is already running and you edited `config.yaml` providers manually
outside `/key` or `hya login`, run `hya serve restart` or refresh via `/key`.
See [Configuration Guide](configuration.md) for every config block, environment
variable, MCP server, permission rule, and compaction setting.

---

## 4. Headless Execution Modes

### Single Turn (`hya exec`)

Run one prompt non-interactively (uses an in-memory store unless `--db` is
given):

```sh
hya exec "summarize this repository's crate boundaries"
hya exec --json "list open TODOs"                   # emit canonical Event JSONL
hya --db ./hya.db exec "audit error handling"       # persist event log to SQLite
```

### Iterative Goal Mode (`hya -p`)

Iterate turns until an independent evaluator model (or `goal.evaluate` hook)
confirms the goal is satisfied or `--max-iterations` is reached:

```sh
hya -p "make all unit tests in hya-proto pass" --max-iterations 6
hya -p "fix clippy warnings" --evaluator-model anthropic/claude-sonnet-4-6
```

### Deterministic / Verifier Loop Mode (`hya loop`)

Iterate toward `--target` gated by a deterministic shell predicate (`--until` or
`--while`) and an independent verifier:

```sh
hya loop --target "all workspace formatting and hya-tool tests pass" \
  --until "cargo fmt --all --check && cargo test -p hya-tool" \
  --budget 8
```

See [Goal and Loop Authoring](goal-loop-authoring.md) and
[Workflows](workflows.md) (`hya workflow list`, `hya workflow run`).

---

## 5. Server, Sessions & Event Replay

### Foreground Server & Daemon Control

```sh
hya serve --bind 127.0.0.1:8080 --db ./hya.db   # foreground HTTP/SSE/WS + gRPC on one port
hya serve start                                 # start detached default daemon
hya serve status                                # inspect URL, PID, uptime, version
hya serve restart                               # clean stop -> start (with session restore prompt)
hya serve stop                                  # graceful drain and stop
```

Both `/v1` HTTP/JSON+SSE+WebSocket and `hya.v1` gRPC are served on the same
port; see the [Protocol Guide](protocol/README.md) and
[API Reference](protocol/api-reference.md).

### Inspect & Replay Sessions

```sh
hya sessions                       # list active root sessions in default DB
hya sessions --all                 # include child subagent and archived sessions
hya tail-session <hysec_...>       # dump canonical Envelope JSONL for debugging
```

---

## 6. Remote Access Over the Secure Relay

Control a workstation or cloud backend from another machine with end-to-end
`Noise_NKpsk0` encryption through an untrusted `hya proxy` relay:

```sh
# 1. On any reachable host (VPS / tunnel / LAN): run the blind relay
hya proxy --port 8766

# 2. On the backend machine: register a Room and print the secret hya:// link
hya serve start --relay http://relay-host:8766

# 3. On your local laptop: connect TUI + WebUI over an encrypted local bridge
hya --connect -     # paste the hya:// link (not echoed)
```

Treat the `hya://…` link like a private key (`hya serve relay rotate` revokes
prior links immediately). See [Secure Relay](relay.md) for TLS, reverse proxies,
and `hya relay doctor`.
