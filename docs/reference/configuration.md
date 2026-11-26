# Configuration Reference

Hya uses two human-edited configuration files, both optional:

1. **Backend & Agent config:** `~/.config/hya/config.yaml` (or `$XDG_CONFIG_HOME/hya/config.yaml`)
2. **TUI layout config:** `~/.config/hya/tui.toml` (or `$XDG_CONFIG_HOME/hya/tui.toml`)

If neither file exists, Hya creates a minimal starter `config.yaml` on first run and starts in offline mode (`hya/offline`) with the `statusline` TUI layout.

---

## 1. Backend Configuration (`config.yaml`)

`config.yaml` is strict YAML. Unknown top-level keys are ignored without crashing startup; run `hya models` or `hya provider list` after manual edits to verify your providers.

### Top-level keys

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `default_model` | `string` | First live model, else `hya/offline` | Default `provider/model` (optional `#effort` suffix) when no agent pin or `--model` flag applies. |
| `default_agent` | `string` | `"hya-main"` | Default root Agent ID (`hya-main`, `hya-plan`, or an installed bundle agent). |
| `reasoning` | `string` | — | Global fallback thinking effort: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`. |
| `providers` | `map<id, Provider>` | `{}` | Model provider routes, base URLs, keys, and model definitions. |
| `agents` | `map<id, AgentPref>` | `{}` | Per-agent pinned `model` and `reasoning` effort for built-in agents. |
| `categories` | `map<name, string[]>` | `{}` | Named ordered failover lists of `provider/model` candidates. |
| `permission` | `PermissionConfig` | `{model: default, rules: []}` | Default permission posture and ordered regex rules. |
| `mcp` | `map<name, McpServer>` | `{}` | Local stdio and remote HTTP MCP tool servers. |
| `plugins` | `map<id, PluginSpec>` | `{}` | Process-wide JSON-RPC stdio plugins. |
| `tools` | `ToolsConfig` | `{}` | Settings for `webfetch`, `websearch`, and `code.mode`. |
| `subagents` | `SubagentLimits` | See table below | Concurrency and per-team turn/mail/member caps. |
| `compaction` | `CompactionConfig` | `{keep_recent: 6, summary_max_tokens: 4096, token_accounting: auto}` | Context-reduction tuning (`shake` tail, handoff cap, accounting mode). |
| `goal` | `{evaluator_model?: string}` | Worker's model | Default judge model for `hya -p` Goal mode. |
| `formatter` | `bool \| map` | `false` | Post-edit auto-formatting (`true` enables 26 built-in language formatters, or pass a map to customize/disable specific ones). |
| `lsp` | `bool \| map` | `true` | Auto-detected stdio language servers (`typescript`, `rust`, `python`, `bash`, `go`, `c`/`cpp`), or `false` to disable. |
| `catalog_scopes` | `{max: usize, idle_ttl_secs: u64}` | `{max: 32, idle_ttl_secs: 1800}` | Cache size and idle timeout for per-Project bundle/plugin overlays. |

---

### `providers.<id>`

```yaml
providers:
  anthropic:
    kind: anthropic
    base_url: https://api.anthropic.com/v1
    api_key: "{env:ANTHROPIC_API_KEY}"
    models:
      - claude-sonnet-4-6
      - id: claude-opus-4-6
        name: Claude Opus 4.6
        reasoning: true
        limit:
          context: 200000
          output: 32000
```

| Field | Type | Description |
| --- | --- | --- |
| `kind` | `string` | Required protocol: `openai` (alias `openai-compatible`, `openai-completion`), `openai-response` (alias `openai-responses`), `openai-codex`, `grok-build`, `anthropic`, or `google`. |
| `base_url` | `string` | Base HTTP(S) URL of the provider API. |
| `api_key` | `string` | Optional inline key, `"{env:VAR_NAME}"`, or `"{file:/abs/path}"`. Overridden if a key is saved in `~/.config/hya/auth/<id>.yaml` via `hya login` or `/key`. |
| `models` | `list` | List of model ID strings or override objects (`{id, name?, limit?: {context?, output?}, reasoning?, modalities?: {input?: [...]}}`). If empty/omitted, Hya fetches `<base_url>/models` into the local model cache. |

---

### `agents.<id>` and Bundle `config.yml`

Pin a built-in agent's model and default reasoning effort in `config.yaml`:

```yaml
agents:
  hya-main:
    model: anthropic/claude-sonnet-4-6
    reasoning: high
  hya-scout:
    model: openai/gpt-5-mini
    reasoning: low
```

For agents owned by a **Bundle**, place the same `agents:` block (plus any custom settings that bundle reads) inside the bundle's own `config.yml`:
- **User/builtin bundle:** `~/.config/hya/bundles/<percent-encoded-bundle-id>/config.yml` (e.g. `hya%2Fplan-impl-review/config.yml`)
- **Project bundle:** `<project>/.hya/bundles/<dir>/config.yml`

---

### `permission`

```yaml
permission:
  model: default   # default | allow | strict | danger
  rules:
    - target: command          # tool | mcp | command
      selector: "^cargo (check|test)\\b"
      permission: allow        # allow | ask | deny
```

| `permission.model` | Behavior |
| --- | --- |
| `default` | Evaluates `rules` in order (last match wins). Unmatched read-only & `task` tools are allowed; edits, MCP calls, web fetches, and `bash` commands ask. |
| `allow` | Any matching `deny` rule blocks; everything else runs without asking (including outside directories). |
| `strict` | Any matching `deny` rule blocks; everything else asks unless previously saved with *Always allow*. |
| `danger` | Allows everything immediately, bypassing even `deny` rules (same as `--yolo`). |

---

### `mcp.<server>`

```yaml
mcp:
  local-stdio:
    command: ["npx", "-y", "@modelcontextprotocol/server-memory"]
    env:
      API_TOKEN: "{env:MY_TOKEN}"
    timeout_ms: 30000
    enabled: true

  remote-http:
    url: https://mcp.example.com/mcp
    transport: sse   # omit for Streamable HTTP (default), or set "sse" for legacy HTTP+SSE
```

---

### `tools`, `subagents`, and `compaction`

```yaml
tools:
  webfetch:
    enabled: true
  websearch:
    enabled: true
    provider: exa          # exa | parallel
    # endpoint: https://mcp.exa.ai/mcp
    # key: "..."
  code:
    mode: normal           # normal | with_tools | only

subagents:
  max_concurrency: 100          # 1..=100 concurrent active streams
  per_run_budget: 1024          # Max live subagents under one root session
  per_team_turn_budget: 1024    # Total subagent turns before runaway stop
  per_team_message_budget: 1024 # Total mail messages before loop stop

compaction:
  keep_recent: 6                # Recent messages/tool steps kept un-evicted by `shake`
  summary_max_tokens: 4096      # Max output tokens for subagent handoff summaries
  token_accounting: auto        # auto | provider | estimate
```

---

## 2. TUI Structure Configuration (`tui.toml`)

`~/.config/hya/tui.toml` controls which UI panels and status bars the TUI draws. Run `/config init` inside Hya to generate a commented template, and `/config reload` after editing.

*(Runtime preferences like color theme, Vim mode, and custom `/keybind` shortcuts are saved automatically by Hya in `~/.config/hya/tui.json`.)*

```toml
version = 1
preset = "statusline"   # minimal | statusline | dock | workspace

[startup]
enabled = true
resources = "compact"   # "compact" | "full"
sections = ["context", "skills", "commands", "mcp", "bundles", "extensions"]
quiet = false           # true shows only warnings/errors

[statusline]
enabled = true
placement = "below"     # "below" | "band"
segments = ["model", "mode", "cwd", "git", "ctx", "tokens"]

[composer]
style = "box"           # "box" | "band"
border_info = []        # e.g. ["model", "mode"] in minimal preset

[composer.paste]
collapse_lines = 10     # Fold pasted text longer than N lines into a chip (0 disables)
collapse_chars = 1000   # Fold pasted text longer than N characters
wrap = true             # Wrap chips in <attachment> tags
image_resize = true     # Auto-convert/downscale large pasted images

[todos]
enabled = false         # Show open todo rows above input in single-column presets
max_rows = 3            # 1..=6

[subagents]
enabled = false         # Show running subagent rows above input

[sidebar]
enabled = false         # true enables the tiled workspace layout

[commands]
skill_commands = true   # Show /skill:<name> entries in the "/" completion menu
```

### Valid statusline segments

`model`, `mode`, `ctx`, `connection`, `agent`, `cwd`, `git`, `tokens`, `session`. When the terminal window narrows, `session`, `tokens`, `git`, `cwd`, and `agent` drop first so `model`, `mode`, `ctx`, and `connection` always remain visible.
