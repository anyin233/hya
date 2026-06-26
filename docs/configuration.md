# Configuration

yaca reads its own YAML config from:

1. `$XDG_CONFIG_HOME/yaca/config.yaml`
2. `$HOME/.config/yaca/config.yaml`

If no usable provider route is configured, yaca falls back to `DevProvider`, the
offline echo provider from [`../crates/yaca-provider/src/dev.rs`](../crates/yaca-provider/src/dev.rs).
The same config file also drives MCP servers, plugins, and formatter status.

## First-Run / Offline Behavior

On startup, yaca tries to load `config.yaml` (see
[`../crates/yaca-app/src/config.rs`](../crates/yaca-app/src/config.rs) `load()`
and `config_path()`). A missing, empty, or provider-less config is **not an
error** — yaca silently falls back to the offline `DevProvider` so the whole
stack stays runnable without API keys. yaca runs offline when any of these hold:

- No `config.yaml` exists at either search path.
- The file exists but is empty.
- It declares no usable provider routes, MCP servers, or plugins.
- A provider has models but no resolvable key (no inline `api_key` and no saved
  `yaca login` token), so it is dropped.

How to tell you are offline:

- The active model id shows as `offline` instead of a real model id.
- `yaca models` prints an empty catalog (no provider routes resolved).
- Assistant replies are prefixed `(yaca dev provider)` and just echo your
  prompt back, e.g. `(yaca dev provider) You said: "..."`.

The fallback is silent. The only message yaca prints is when a config file is
present but fails to parse — then it logs to stderr and still continues
offline:

```text
yaca: config error (...); using the offline provider
```

To leave offline mode, configure at least one provider with a resolvable key
(see [Providers](#providers) and [Auth Tokens](#auth-tokens)).

## Sample `config.yaml`

A copy-paste starting point covering a default model, a live provider, an MCP
server, and a plugin. Remove the parts you do not need; every top-level section
is optional.

```yaml
# ~/.config/yaca/config.yaml  (or $XDG_CONFIG_HOME/yaca/config.yaml)

# Model used when neither `--model` nor `YACA_MODEL` is set. Must be served by
# one of the providers below. If omitted, yaca prefers a model whose id
# contains "sonnet", otherwise the first configured model.
default_model: claude-sonnet-4-6

# Optional: agent profile selected when a workdir does not specify one.
# Falls back to the built-in `build` agent when omitted.
default_agent: build

# Each entry under `providers.<id>` becomes one HTTP route. The <id> is also the
# name used by `yaca login <id>` and shown as the provider in model refs.
providers:
  anthropic:
    kind: anthropic                      # openai | openai-compatible | anthropic | google
    base_url: https://api.anthropic.com/v1
    # Inline key is optional. Forms: literal, {env:VAR}, or {file:/path}.
    # A token saved via `yaca login anthropic <token>` takes precedence.
    api_key: "{env:ANTHROPIC_API_KEY}"
    models: [claude-sonnet-4-6]          # providers with no models are skipped

# MCP servers. Tools are registered as mcp__<server>__<tool>.
mcp:
  filesystem:
    command: [node, /path/to/server.js]  # stdio command for the server process
    env:
      TOKEN: "{env:MCP_TOKEN}"           # env values also accept {env:}/{file:}
    timeout_ms: 1000
    # enabled: false                     # set to skip this server

# Plugins. May also be discovered from <workdir>/.yaca/plugins/**/plugin.toml.
plugins:
  memory:
    command: [python3, memory.py]        # stdio JSON-RPC process
    timeout_ms: 500
    env:
      TOKEN: literal-token
  opencode:
    kind: opencode                       # rust (default) | opencode | other
```

## Providers

Each entry under `providers` builds one HTTP route:

```yaml
default_model: claude-sonnet-4-6
providers:
  anthropic:
    kind: anthropic
    base_url: https://api.anthropic.com/v1
    api_key: "{env:ANTHROPIC_API_KEY}"
    models: [claude-sonnet-4-6]
  gateway:
    kind: openai-compatible
    base_url: https://gateway.example/v1
    api_key: "{file:/run/secrets/gateway-key}"
    models: [gpt-5.5, gpt-5.4]
  google:
    kind: google
    base_url: https://generativelanguage.googleapis.com
    api_key: literal-secret
    models: [gemini-2.0-flash]
```

Supported `kind` values:

| `kind` | Route |
| --- | --- |
| `openai` or `openai-compatible` | OpenAI Chat Completions compatible route. |
| `anthropic` | Anthropic Messages route. |
| `google` | Gemini route. |

Providers without models are skipped. Providers without an inline `api_key` are
still valid if a saved token exists for that provider id.

## Auth Tokens

`api_key` accepts:

```yaml
api_key: literal-secret
api_key: "{env:MY_PROVIDER_API_KEY}"
api_key: "{file:/absolute/path/to/key.txt}"
```

Saved tokens take precedence over inline `api_key` values:

```sh
yaca login anthropic "$ANTHROPIC_API_KEY"
yaca auth list
yaca auth logout anthropic
```

Tokens are stored under `~/.config/yaca/auth/<provider>.yaml`. HTTP auth headers
are marked sensitive and redirects are disabled so a secret is not forwarded to
another host.

## Model Selection

The active model is selected in this order:

1. `--model <id>` CLI flag.
2. `YACA_MODEL` environment variable.
3. `default_model` from `config.yaml`.
4. A configured model whose id contains `sonnet`.
5. The first configured model id.
6. `offline` when using the development provider.

Examples:

```sh
YACA_MODEL=claude-sonnet-4-6 yaca
yaca --model gpt-5.5 exec "summarize the architecture"
yaca models
yaca models gateway --verbose
```

The selected model must be served by one configured route. If no route reports
capabilities for the model, the router returns `unknown provider for model`.

## Environment Variables

yaca reads the following `YACA_*` variables (verified against the source listed
in each row). Unset variables fall back to the documented default. Beyond these,
yaca honors the standard `HOME` and `XDG_CONFIG_HOME` for config/auth paths.

| Variable | Effect | Default | Source |
| --- | --- | --- | --- |
| `YACA_MODEL` | Active model id when `--model` is not passed and no `default_model` resolves. | `default_model`, else a `sonnet` model, else the first model, else `offline`. | `crates/yaca-app/src/config.rs`, `crates/yaca-app/src/runtime.rs` |
| `YACA_COMPACTION_THRESHOLD` | Token count that triggers context compaction. Parsed as a number; unparseable values are ignored. | `CompactionConfig::default().token_threshold` | `crates/yaca-app/src/runtime.rs` (`compaction_config`) |
| `YACA_COMPACTION_KEEP_RECENT` | Number of most-recent messages kept verbatim when compacting. Parsed as a number; unparseable values are ignored. | `CompactionConfig::default().keep_recent` | `crates/yaca-app/src/runtime.rs` (`compaction_config`) |
| `YACA_HISTORY_DIR` | Directory for the TUI's JSONL session history. | `$HOME/.yaca/history`, else a temp dir. | `crates/yaca-cli/src/tui/history.rs` |
| `YACA_EXPORT_DIR` | Directory where `/export` writes Markdown transcripts. | `$HOME/.yaca/exports`, else a temp dir. | `crates/yaca-cli/src/tui.rs` (`export_root`) |
| `YACA_OPENCODE_ADAPTER_DIR` | Path to an alternate OpenCode plugin adapter checkout (used for `kind: opencode` plugins). | Bundled adapter in `crates/yaca-plugin-opencode/adapter`. | `crates/yaca-app/src/plugins.rs` |
| `YACA_HYA_BIN` | Path to the `hya` binary spawned by `yaca serve` integrations. | Newest sibling build, else `hya` on `PATH`. | `crates/yaca-cli/src/serve.rs` (`resolve_hya_bin`) |

Related, non-`YACA_` variables that also affect behavior:

| Variable | Effect | Source |
| --- | --- | --- |
| `BUN` | Bun binary used to run the bundled OpenCode adapter. | `crates/yaca-app/src/plugins.rs` |
| `OPENCODE_WEBSEARCH_PROVIDER` | Selects the web-search backend used by the websearch tool. | `crates/yaca-tool/src/websearch.rs` |
| `PARALLEL_API_KEY`, `EXA_API_KEY` | API keys for the corresponding websearch providers. | `crates/yaca-tool/src/websearch.rs` |

## MCP Servers

MCP servers are configured under `mcp`:

```yaml
mcp:
  filesystem:
    command: [node, /path/to/server.js]
    env:
      TOKEN: "{env:MCP_TOKEN}"
    timeout_ms: 1000
  disabled-example:
    enabled: false
    command: [node, server.js]
```

Enabled servers are started during runtime composition. Their tools are
registered as `mcp__<server>__<tool>` and use the normal permission plane.
`GET /mcp` reports connected, disabled, and failed servers in an
OpenCode-shaped status response. Dynamic HTTP MCP add/connect/disconnect routes
exist for compatibility, but they do not durably rewrite `config.yaml` or hot-plug
new tools into an already running engine.

## Plugins

Plugins may be declared directly in config or discovered from
`<workdir>/.yaca/plugins/**/plugin.toml`:

```yaml
plugins:
  memory:
    command: [python3, memory.py]
    timeout_ms: 500
    env:
      TOKEN: literal-token
  opencode:
    kind: opencode
```

Config entries support:

| Field | Meaning |
| --- | --- |
| `kind` | `rust`, `opencode`, or `other`; default is `rust`. |
| `command` | Process command for stdio JSON-RPC. |
| `enabled` | Defaults to `true`; disabled entries are skipped. |
| `timeout_ms` | Optional request timeout. |
| `env` | Environment variables passed to the plugin process as configured. |

For `kind: opencode` entries without `command`, yaca uses the bundled Bun
adapter from `crates/yaca-plugin-opencode/adapter`. Set `BUN` to choose a Bun
binary or `YACA_OPENCODE_ADAPTER_DIR` to point at an alternate adapter checkout.
If Bun is not available, that plugin is skipped.

The plugin host supports registered tools, command/message/text/chat hooks,
event notifications, permission hooks, shell/tool hooks, and workspace adapter
metadata.

## Formatter

The `formatter` key controls the formatter plane exposed through tools and the
OpenCode-compatible `/formatter` route:

```yaml
formatter: true
```

enables built-in formatters. A map configures custom commands:

```yaml
formatter:
  treefmt:
    command: [treefmt, "$FILE"]
    extensions: [.nix]
  gofmt:
    disabled: true
```

Custom entries support `disabled`, `command`, `environment`, and `extensions`.
The formatter runs after successful `write`, `edit`, and `apply_patch` tool
operations when a matching provider entry is available.

## Custom Commands

The TUI loads markdown prompt commands from:

1. `$HOME/.config/opencode/commands/*.md`
2. `$HOME/.config/opencode/command/*.md`
3. `$HOME/.config/yaca/prompts/*.md`
4. `<workdir>/.opencode/commands/*.md`
5. `<workdir>/.opencode/command/*.md`
6. `<workdir>/.yaca/prompts/*.md`

Project commands override user commands with the same file stem. The file stem
becomes the slash command name. Optional frontmatter fields are parsed:

```markdown
---
description: Create a component
agent: build
model: claude-sonnet-4-6
---
Create $1 in $2.

All args: $ARGUMENTS
```

Expanded command bodies are submitted as normal prompts. If `agent` names a
built-in TUI profile, yaca applies that profile before the turn starts. If
`model` is present, yaca switches the submitted turn to that model.
