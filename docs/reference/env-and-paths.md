# Environment Variables & File Paths

Hya follows standard XDG directories on Linux and macOS, so your config, chat database, and caches never litter your home folder.

---

## 1. Where Hya stores files on disk

| What | Path (when `XDG_*` is unset) | XDG override | Notes |
| --- | --- | --- | --- |
| **Main config** | `~/.config/hya/config.yaml` | `$XDG_CONFIG_HOME/hya/config.yaml` | Providers, models, permissions, MCP, compaction. |
| **TUI layout** | `~/.config/hya/tui.toml` | `$XDG_CONFIG_HOME/hya/tui.toml` | Declarative UI preset & statusline (`$HYA_TUI_TOML` overrides). |
| **TUI saved state** | `~/.config/hya/tui.json` | `$XDG_CONFIG_HOME/hya/tui.json` | Saved theme, Vim toggle, custom `/keybind` keys, pane layout (`$HYA_TUI_CONFIG` overrides). |
| **Saved API keys & OAuth** | `~/.config/hya/auth/<provider>.yaml` | `$XDG_CONFIG_HOME/hya/auth/` | Written with `0600` (owner-only) permissions. |
| **User bundle configs** | `~/.config/hya/bundles/<id>/config.yml` | `$XDG_CONFIG_HOME/hya/bundles/` | `<id>` is percent-encoded, e.g. `hya%2Fcode-mode`. |
| **User skills / commands / workflows** | `~/.config/hya/{skills,commands,workflows}/` | `$XDG_CONFIG_HOME/hya/...` | Shared across all projects on your machine. |
| **Default session DB** | `~/.local/state/hya/sessions.db` | `$XDG_STATE_HOME/hya/sessions.db` | SQLite WAL event log, Projects, and saved permission rules. |
| **Daemon discovery & lock** | `~/.local/state/hya/sessions.db.{server.json,runtime-owner.lock,server.log}` | Beside `--db` | Tracks the live daemon URL/PID and single-writer lock. |
| **Relay identity** | `~/.local/state/hya/sessions.db.relay-identity.json` | Beside `--db` | Room Ed25519 key, Noise X25519 key, and link PSK (`0600`). |
| **Launcher log** | `~/.local/state/hya/hya.log` | `$XDG_STATE_HOME/hya/hya.log` | Output from bare `hya` and `[webui]` (rotates at 4 MiB). |
| **Installed user bundles** | `~/.local/share/hya/bundles/registry.sqlite3` | `$XDG_DATA_HOME/hya/bundles/` | User-scope `.hyabundle` registry. |
| **Model list cache** | `~/.cache/hya/model_cache.db` | `$XDG_CACHE_HOME/hya/model_cache.db` | Remote provider model lists (`hya models --refresh` updates). |
| **Temporary sessions** | `~/.cache/hya/scratch/<session_id>/` | `$XDG_CACHE_HOME/hya/scratch/` | Isolated working folders for `/new --temp` chats. |

### Project-local folder (`<project-root>/.hya/`)

Inside any repository registered as a Project root, Hya discovers:
- `<root>/AGENTS.md` — Project instructions
- `<root>/.hya/skills/<name>/SKILL.md` (and `<root>/.agents/skills/<name>/SKILL.md`) — Project skills
- `<root>/.hya/commands/**/*.md` — Project slash commands
- `<root>/.hya/workflows/*.hya.md` — Project Workflows
- `<root>/.hya/bundles/<dir>/` — Project bundles (and their `config.yml`)
- `<root>/.hya/plugins/<name>/plugin.toml` — Project plugins

---

## 2. Environment variables

### Everyday runtime & model overrides

| Variable | Default | Meaning |
| --- | --- | --- |
| `HYA_MODEL` | `config.yaml` `default_model` | Default model when `--model` is not passed on the CLI. |
| `HYA_RELAY_LINK` | — | Secret `hya://...` link read by `hya --connect` and `hya bridge` when `-` is not used (scrubbed from child environments after reading). |
| `HYA_TUI_TOML` | `~/.config/hya/tui.toml` | Explicit path to `tui.toml`. |
| `HYA_TUI_CONFIG` | `~/.config/hya/tui.json` | Explicit path to `tui.json`. |
| `SSL_CERT_FILE` | System + WebPKI roots | Custom PEM CA bundle for outbound HTTPS provider calls behind corporate proxies. |
| `VISUAL` / `EDITOR` | `vi` | External text editor opened by `/editor` and `/config edit`. |

### Context, compaction & subagent overrides (env wins over the config files)

| Variable | Default | Overrides |
| --- | --- | --- |
| `HYA_COMPACTION_KEEP_RECENT` | `6` | `compaction.keep_recent` in `hya/compaction`'s `config.yml` (read by the bundle) |
| `HYA_COMPACTION_SUMMARY_MAX_TOKENS` | `4096` | `compaction.summary_max_tokens` in `hya/compaction`'s `config.yml` |
| `HYA_TOKEN_ACCOUNTING` | `auto` | `config.yaml` `context.token_accounting` (`auto \| provider \| estimate`); an unknown value is ignored |
| `HYA_SUBAGENT_MAX_CONCURRENCY` | `100` | `subagents.max_concurrency` |
| `HYA_SUBAGENT_BUDGET` | `1024` | `subagents.per_run_budget` |
| `HYA_SUBAGENT_TURN_BUDGET` | `1024` | `subagents.per_team_turn_budget` |
| `HYA_SUBAGENT_MESSAGE_BUDGET` | `1024` | `subagents.per_team_message_budget` |

### Installer variables (`install.sh`)

| Variable | Default | Meaning |
| --- | --- | --- |
| `HYA_VERSION` | Latest stable | Version tag to install (same as `--version`). |
| `HYA_INSTALL_DIR` | `$HOME/.local` | Target prefix directory (same as `--prefix`). |
| `HYA_RELEASES_URL` | `https://github.com/chrono-foundry/hya/releases` | Mirror URL for release archives and `SHA256SUMS`. |
| `HYA_REPO` | `chrono-foundry/hya` | GitHub `owner/repo` used to construct the default releases URL. |
| `HYA_TARGET` | Auto-detected | Override target triple (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`). |

### Advanced, developer & diagnostic variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `BUN` | Bundled `lib/hya/bin/bun`, else `PATH` | Path to the `bun` executable used for TUI, WebUI, and Bun extensions. |
| `HYA_BIN` | `PATH` | Path to `hya` binary used when launching `packages/hya-tui` directly from source. |
| `HYA_TUI_DIR` / `HYA_TUI_WEB_DIR` | `<prefix>/lib/hya/{tui,tui-web}` | Override frontend package directories for bare `hya`. |
| `HYA_BUN_ADAPTER_DIR` / `HYA_CLAUDE_ADAPTER_DIR` | `<prefix>/lib/hya/{bun,claude}-adapter` | Override adapter directories. |
| `HYA_GRPC_BIND` | — | Optional extra `<host:port>` gRPC listener (in addition to single-port gRPC on `--bind`). |
| `HYA_DEFER_SIDEPLANES` | `1` (deferred) | Set to `0` or `false` to block server readiness until all MCP servers finish connecting. |
| `HYA_EVENT_BUS_CAPACITY` | `8192` | In-memory live broadcast ring buffer size. |
| `HYA_STARTUP_TRACE` | off | Set to `1` or `true` to print JSON startup timing phases on `stderr`. |
| `HYA_STARTUP_TRACE_FILE` | — | Append Backend + TUI startup timing marks as JSONL to this file. |
| `HYA_BUNDLE_CONFIG_DIR` / `HYA_BUNDLE_CONFIG_FILE` | Set by Hya | Passed into bundle/plugin child processes so they can read their `config.yml`. |
| `HYA_TERMINAL` | `1` *(output only)* | Set inside Hya PTY shells so child programs know they are inside Hya. |
