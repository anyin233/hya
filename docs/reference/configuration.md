# Configuration Reference

Hya v2 uses three human-edited configuration surfaces, all optional (auto-created with safe defaults when absent):

1. **Core Backend & Agent config:** `~/.config/hya/config.yaml` (or `$XDG_CONFIG_HOME/hya/config.yaml`, requires `schema_version: 2`)
2. **Per-Bundle config:** `~/.config/hya/bundles/<percent-encoded-bundle-id>/config.yml` (or `<project>/.hya/bundles/<dir>/config.yml`)
3. **TUI structure & preferences:** `~/.config/hya/tui.toml` (`version = 2`) and `~/.config/hya/tui.json` (`paneLayout` `version: 4`)

If neither `config.yaml` nor `tui.toml` exists, Hya creates a starter `schema_version: 2` `config.yaml` on first run and starts in offline mode (`hya/offline`) with the `statusline` TUI layout. If an unmigrated v1 `config.yaml` is present, Hya fails fast and directs you to run `hya-migration run` (see **[Migrating from Hya v1 to v2](../guide/migration-v1-to-v2.md)**).

---

## 1. Core Backend Configuration (`config.yaml` v2)

`config.yaml` is strict YAML and **requires `schema_version: 2`**. Run `hya models` or `hya provider list` after manual edits to verify your providers.

```yaml
schema_version: 2
default_model: hya/offline
providers: {}
mcp: {}
plugins: {}
bundles:
  builtin:
    profile: full    # full | bare
    enable: []
    disable: []
permission:
  model: default
  rules: []
```

### Top-level keys

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `schema_version` | `u32` | **Required (`2`)** | Must be `2`. Legacy v1 configs are rejected at startup until migrated with `hya-migration run`. |
| `default_model` | `string` | First live model, else `hya/offline` | Default `provider/model` (optional `#effort` suffix) when no agent pin or `--model` flag applies. |
| `default_agent` | `string` | The `default_agent` of the enabled bundle that declares one (`hya-main` from `hya/core-agents`), else `"hya-bare"` (for example under `--bare`) | Default root Agent ID (`hya-main`, `hya-plan`, or an installed bundle agent). |
| `reasoning` | `string` | — | Global fallback thinking effort: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`. |
| `providers` | `map<id, Provider>` | `{}` | Model provider routes, base URLs, keys, retry budgets, and model definitions. |
| `provider_retry` | `ProviderRetry` | Defaults | Default retry/replay budget for every provider route (overridden per provider or via `HYA_PROVIDER_RETRY_*`). |
| `agents` | `map<id, AgentPref>` | `{}` | Per-agent pinned `model` and `reasoning` effort, by Agent id whatever bundle provides the Agent. |
| `categories` | `map<name, string[]>` | `{}` | Named ordered failover lists of `provider/model` candidates. |
| `bundles` | `BundlesConfig` | `{builtin: {profile: full, enable: [], disable: []}}` | Controls which of the 15 built-in bundles under `bundles/builtin/` load at startup. |
| `permission` | `PermissionConfig` | `{model: default, rules: []}` | Default permission posture and ordered regex rules. |
| `mcp` | `map<name, McpServer>` | `{}` | Local stdio and remote HTTP MCP tool servers. |
| `plugins` | `map<id, PluginSpec>` | `{}` | Process-wide JSON-RPC stdio plugins. |
| `tools` | `{code?: {mode: normal \| with_tools \| only}}` | `{code: {mode: normal}}` | Core Code Mode (`packages/hya-code-mode`) mode setting. *(Web fetch/search settings now live in `bundles/hya%2Fnetwork/config.yml`.)* |
| `catalog_scopes` | `{max: usize, idle_ttl_secs: u64}` | `{max: 32, idle_ttl_secs: 1800}` | Cache size and idle timeout for per-Project bundle/plugin overlays. |
| `context` | `{token_accounting: auto \| provider \| estimate}` | `{token_accounting: auto}` | How the kernel measures context-window occupancy: `auto` trusts a route's reported usage while it stays plausible, `provider` always trusts it, `estimate` always counts locally. Applies whatever bundles are enabled; `HYA_TOKEN_ACCOUNTING` wins. An unknown value or key fails startup. |

---

### `bundles.builtin` (Composing `hya-bare` + 15 Built-in Bundles)

```yaml
schema_version: 2
bundles:
  builtin:
    profile: full          # "full" (all 15 enabled by default) | "bare" (zero enabled by default)
    enable: []             # Extra built-in IDs to enable on top of `profile: bare`
    disable:               # Built-in IDs to disable when `profile: full`
      - hya/network        # Air-gapped operation (removes webfetch/websearch)
      - hya/hashline       # Fall back to standard hya-bare read/write/edit/grep
```

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `bundles.builtin.profile` | `"full" \| "bare"` | `"full"` | Base built-in bundle profile. Passing `--bare` on the CLI overrides this to `bare` with empty `enable`. |
| `bundles.builtin.enable` | `string[]` | `[]` | Built-in bundle IDs to enable selectively (for example `profile: bare` with `enable: [hya/loop]`). |
| `bundles.builtin.disable` | `string[]` | `[]` | Built-in bundle IDs to disable selectively (for example `[hya/network]` for air-gapped environments or `[hya/hashline]` to use standard core file tools). |

**Valid built-in bundle IDs (15):** `hya/hashline`, `hya/coding-extras`, `hya/multi-agent`, `hya/network`, `hya/agent-channels`, `hya/compaction`, `hya/plan`, `hya/goal`, `hya/loop`, `hya/workflow`, `hya/workspace-files`, `hya/core-agents`, `hya/core-skills`, `hya/core-commands`, `hya/basic-tui-components`.

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
| `kind` | `string` | Required protocol: `openai` (alias `openai-compatible`, `openai-completion`), `openai-response`, `openai-codex`, `grok-build`, `anthropic`, or `google`. |
| `base_url` | `string` | Base HTTP(S) URL of the provider API. |
| `api_key` | `string` | Optional inline key, `"{env:VAR_NAME}"`, or `"{file:/abs/path}"`. Overridden if a key is saved in `~/.config/hya/auth/<id>.yaml` via `hya login` or `/key`. |
| `models` | `list` | List of model ID strings or override objects (`{id, name?, limit?: {context?, output?}, reasoning?, modalities?: {input?: [...]}}`). If empty/omitted, Hya fetches `<base_url>/models` into the local model cache. |

---

### `agents.<id>`

Pin an agent's model and default reasoning effort in `config.yaml`. An entry applies by Agent id whatever bundle provides the Agent (`hya-main` and `hya-title` come from `hya/core-agents`); without one, the `agents.<id>.model` of the owning bundle's `config.yml` applies. Setting a model from the TUI updates the file that already holds the Agent's entry, else the owning bundle's `config.yml`:

```yaml
agents:
  hya-main:
    model: anthropic/claude-sonnet-4-6
    reasoning: high
  hya-scout:
    model: openai/gpt-5-mini
    reasoning: low
```

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

### `mcp.<server>` & `tools.code`

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

tools:
  code:
    mode: normal     # normal | with_tools | only
```

---

## 2. Per-Bundle Configuration (`config.yml`)

In Hya v2, every built-in and installed bundle reads its own domain configuration and per-bundle agent pins from a dedicated `config.yml` (loaded via `HostCatalogReader::bundle_config(bundle_id)` for native ABI v2 bundles or `HYA_BUNDLE_CONFIG_FILE` for JS bundles):

- **User & built-in bundles:** `~/.config/hya/bundles/<percent-encoded-bundle-id>/config.yml` (`/` encodes as `%2F`)
- **Project bundles:** `<project>/.hya/bundles/<dir>/config.yml`

| Built-in Bundle | Config Path | Keys & Defaults |
| --- | --- | --- |
| `hya/network` | `~/.config/hya/bundles/hya%2Fnetwork/config.yml` | `webfetch: { enabled: true, max_response_bytes: 5242880, default_timeout_secs: 30, max_timeout_secs: 120 }`, `websearch: { enabled: true, provider: exa \| parallel, endpoint?, key? }` |
| `hya/coding-extras` | `~/.config/hya/bundles/hya%2Fcoding-extras/config.yml` | `formatter: false \| true \| map<name, { command, extensions, environment?, disabled? }>`, `lsp: true \| false \| map<name, { command, extensions, disabled?, env?, initialization? }>` |
| `hya/compaction` | `~/.config/hya/bundles/hya%2Fcompaction/config.yml` | `compaction: { keep_recent: 6, summary_max_tokens: 4096 }` (`token_accounting` moved to `config.yaml` `context.token_accounting`; leaving it here fails startup) |
| `hya/goal` | `~/.config/hya/bundles/hya%2Fgoal/config.yml` | `goal: { evaluator_model?: "provider/model" }` |
| `hya/loop` | `~/.config/hya/bundles/hya%2Floop/config.yml` | `loop: { evaluator_model?: "provider/model" }` |
| `hya/multi-agent` | `~/.config/hya/bundles/hya%2Fmulti-agent/config.yml` | `subagents: { max_concurrency: 100, per_run_budget: 1024, per_team_turn_budget: 1024, per_team_message_budget: 1024 }`, plus optional `agents:` pins |
| `hya/workflow` | `~/.config/hya/bundles/hya%2Fworkflow/config.yml` | Optional `agents:` pins for workflow stage agents |

Example (`~/.config/hya/bundles/hya%2Fnetwork/config.yml`):

```yaml
webfetch:
  enabled: true
websearch:
  enabled: true
  provider: exa          # exa | parallel
  # endpoint: https://mcp.exa.ai/mcp
  # key: "{env:EXA_API_KEY}"
```

---

## 3. TUI Structure Configuration (`tui.toml` v2 & `tui.json`)

`~/.config/hya/tui.toml` controls which UI panels and status bars the TUI draws (`version = 2`). Run `/config init` inside Hya to generate a commented template, and `/config reload` after editing.

*(Runtime preferences like color theme, Vim mode, custom `/keybind` shortcuts, and saved n-ary `paneLayout` trees (`version: 4`) are stored in `~/.config/hya/tui.json`.)*

```toml
version = 2
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

[panes."hya/multi-agent#subagents"]
enabled = false         # Dynamic bundle pane contributed by hya/multi-agent

[sidebar]
enabled = false         # true enables the tiled workspace layout

[commands]
skill_commands = true   # Show /skill:<name> entries in the "/" completion menu
```

### Valid statusline segments

`model`, `mode`, `ctx`, `connection`, `agent`, `cwd`, `git`, `tokens`, `session`. When the terminal window narrows, `session`, `tokens`, `git`, `cwd`, and `agent` drop first so `model`, `mode`, `ctx`, and `connection` always remain visible.
