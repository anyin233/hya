# Configuration

yaca reads its own YAML config from:

1. `$XDG_CONFIG_HOME/yaca/config.yaml`
2. `$HOME/.config/yaca/config.yaml`

If no usable provider route is configured, yaca falls back to `DevProvider`, the
offline echo provider from [`../crates/yaca-provider/src/dev.rs`](../crates/yaca-provider/src/dev.rs).
The same config file also drives MCP servers, plugins, and formatter status.

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
