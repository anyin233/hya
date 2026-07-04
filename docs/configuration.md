# Configuration

hya reads its own YAML config from:

1. `$XDG_CONFIG_HOME/hya/config.yaml`
2. `$HOME/.config/hya/config.yaml`

If no usable provider route is configured, hya falls back to `DevProvider`, the
offline echo provider from [`../crates/hya-provider/src/dev.rs`](../crates/hya-provider/src/dev.rs).
The same config file also drives MCP servers, plugins, and formatter status.

## First-Run / Offline Behavior

On startup, hya tries to load `config.yaml` (see
[`../crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs) `load()`
and `config_path()`). `cargo build` only compiles the workspace and does not
write user config. When the `hya` frontend or a `hya-backend` command starts
and no file exists, hya creates the config directory and writes a starter
`config.yaml` before resolving runtime config:

```yaml
default_model: offline
providers: {}
mcp: {}
plugins: {}
```

A missing, empty, or provider-less config is **not an error** — hya falls back
to the offline `DevProvider` so the whole stack stays runnable without API keys.
hya runs offline when any of these hold:

- No usable provider route exists in `config.yaml`.
- The file exists but is empty.
- It declares no usable provider routes, MCP servers, or plugins.
- A provider has models but no resolvable key (no inline `api_key` and no saved
  `hya-backend login` token), so it is dropped.

When an interactive TUI startup creates the starter file for the first time, it
prompts before doing anything else:

```text
hya: import Compat model config now? [y/N]
```

Answering yes imports provider base URLs, model IDs, and API key values or
templates from the first discovered Compat config (`$COMPAT_CONFIG`,
`$XDG_CONFIG_HOME/compat/{opencode.json,config.json,opencode.jsonc}`,
`$HOME/.config/opencode/{...}`, then `$HOME/.opencode/{...}`). The import is
local and does not print secret values. If no importable Compat provider has
both a base URL and at least one model, hya keeps the starter config and
continues offline.

How to tell you are offline:

- The active model id shows as `offline` instead of a real model id.
- `hya-backend models` prints an empty catalog (no provider routes resolved).
- Assistant replies are prefixed `(hya dev provider)` and just echo your
  prompt back, e.g. `(hya dev provider) You said: "..."`.

Non-interactive commands create the starter file without prompting and keep
machine-readable stdout clean. The only runtime config message they print is
when a config file is present but fails to parse — then hya logs to stderr and
still continues offline:

```text
hya: config error (...); using the offline provider
```

To leave offline mode, configure at least one provider with a resolvable key
(see [Providers](#providers) and [Auth Tokens](#auth-tokens)).

## Sample `config.yaml`

A copy-paste starting point covering a default model, a live provider, an MCP
server, and a plugin. Remove the parts you do not need; every top-level section
is optional.

```yaml
# ~/.config/hya/config.yaml  (or $XDG_CONFIG_HOME/hya/config.yaml)

# Model used when neither `--model` nor `HYA_MODEL` is set. Must be served by
# one of the providers below. If omitted, hya prefers a model whose id
# contains "sonnet", otherwise the first configured model.
default_model: claude-sonnet-4-6

# Optional: agent profile selected when a workdir does not specify one.
# Falls back to the built-in `build` agent when omitted.
default_agent: build

# Each entry under `providers.<id>` becomes one HTTP route. The <id> is also the
# name used by `hya-backend login <id>` and shown as the provider in model refs.
providers:
  anthropic:
    kind: anthropic                      # openai | openai-compatible | anthropic | google
    base_url: https://api.anthropic.com/v1
    # Inline key is optional. Forms: literal, {env:VAR}, or {file:/path}.
    # A token saved via `hya-backend login anthropic <token>` takes precedence.
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

# Plugins. May also be discovered from <workdir>/.hya/plugins/**/plugin.toml.
plugins:
  memory:
    command: [python3, memory.py]        # stdio JSON-RPC process
    timeout_ms: 500
    env:
      TOKEN: literal-token
  compat:
    kind: compat                       # rust (default) | compat | other
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
hya-backend login anthropic "$ANTHROPIC_API_KEY"
hya-backend auth list
hya-backend auth logout anthropic
```

Tokens are stored under `~/.config/hya/auth/<provider>.yaml`. HTTP auth headers
are marked sensitive and redirects are disabled so a secret is not forwarded to
another host.

## Model Selection

The active model is selected in this order:

1. `--model <id>` CLI flag.
2. `HYA_MODEL` environment variable.
3. `default_model` from `config.yaml`.
4. A configured model whose id contains `sonnet`.
5. The first configured model id.
6. `offline` when using the development provider.

Examples:

```sh
HYA_MODEL=claude-sonnet-4-6 hya
hya-backend --model gpt-5.5 exec "summarize the architecture"
hya-backend models
hya-backend models gateway --verbose
```

The selected model must be served by one configured route. If no route reports
capabilities for the model, the router returns `unknown provider for model`.

## Environment Variables

hya reads the following `HYA_*` variables (verified against the source listed
in each row). Unset variables fall back to the documented default. Beyond these,
hya honors the standard `HOME` and `XDG_CONFIG_HOME` for config/auth paths.

| Variable | Effect | Default | Source |
| --- | --- | --- | --- |
| `HYA_MODEL` | Active model id when `--model` is not passed and no `default_model` resolves. | `default_model`, else a `sonnet` model, else the first model, else `offline`. | `crates/hya-app/src/config.rs`, `crates/hya-app/src/runtime.rs` |
| `HYA_COMPACTION_THRESHOLD` | Token count that triggers context compaction. Parsed as a number; unparseable values are ignored. | `CompactionConfig::default().token_threshold` | `crates/hya-app/src/runtime.rs` (`compaction_config`) |
| `HYA_COMPACTION_KEEP_RECENT` | Number of most-recent messages kept verbatim when compacting. Parsed as a number; unparseable values are ignored. | `CompactionConfig::default().keep_recent` | `crates/hya-app/src/runtime.rs` (`compaction_config`) |
| `HYA_COMPAT_ADAPTER_DIR` | Path to an alternate Compat plugin adapter checkout (used for `kind: compat` plugins). | Bundled adapter in `crates/hya-plugin-compat/adapter`. | `crates/hya-app/src/plugins.rs` |
| `HYA_FRONTEND_BIN` | Path to the `hya` binary spawned by `hya-backend` frontend integrations. | Newest sibling build, else `hya` on `PATH`. | `crates/hya-backend/src/serve.rs` (`resolve_hya_bin`) |

Related, non-`HYA_` variables that also affect behavior:

| Variable | Effect | Source |
| --- | --- | --- |
| `BUN` | Bun binary used to run the bundled Compat adapter. | `crates/hya-app/src/plugins.rs` |
| `COMPAT_WEBSEARCH_PROVIDER` | Selects the web-search backend used by the websearch tool. | `crates/hya-tool/src/websearch.rs` |
| `PARALLEL_API_KEY`, `EXA_API_KEY` | API keys for the corresponding websearch providers. | `crates/hya-tool/src/websearch.rs` |

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
Compat-shaped status response. Dynamic HTTP MCP add/connect/disconnect routes
exist for compatibility, but they do not durably rewrite `config.yaml` or hot-plug
new tools into an already running engine.

### Compat migration into hya

Interactive first-run startup can import Compat provider/model config into
`config.yaml`. You can also run the model import explicitly without starting a
TUI:

```sh
hya --import compat
```

The explicit import currently supports only Compat and only model/provider
config. It replaces `default_model` and `providers` in `config.yaml`, while
preserving existing non-model sections such as `mcp`, `plugins`, and
`default_agent`. The command prints TODO placeholders for skills and MCP import;
future sources such as Codex and Claude are reserved but not implemented yet.

To mirror Compat-owned MCP and skill surfaces into the default hya runtime,
use the workspace xtask migration entrypoint:

```sh
cargo run -p xtask -- sync-compat --help
```

The first-pass migration contract is intentionally narrow:

- Compat remains the canonical source of truth.
- The migration supports Compat local stdio MCP entries that map to hya's
  `McpServerConfig` shape. The Compat `command`, `enabled`, and `environment`
  fields are migrated; `environment` becomes the hya `env` map and any
  `{env:VAR}` / `{file:path}` templates are preserved verbatim. Compat
  remote MCP entries are skipped in this first pass.
- The migration materializes skills into the hya skill root as managed symlinks.
- The migration writes a managed-state lock file at
  `~/.config/hya/compat-sync-lock.json` so rerun and prune operations can be
  safe and idempotent.
- Compat provider/model sections are handled by the first-run import prompt,
  not this xtask. The xtask focuses on MCP and skills.

Typical workflow:

```sh
cargo run -p xtask -- sync-compat \
  --dry-run \
  --compat-config "$HOME/.config/opencode/opencode.json" \
  --compat-skill-root .opencode/skills \
  --hya-config "$HOME/.config/hya/config.yaml" \
  --hya-skills-root "$HOME/.config/hya/skills"

cargo run -p xtask -- sync-compat \
  --compat-config "$HOME/.config/opencode/opencode.json" \
  --compat-skill-root .opencode/skills \
  --hya-config "$HOME/.config/hya/config.yaml" \
  --hya-skills-root "$HOME/.config/hya/skills"
```

Repeat `--compat-skill-root <PATH>` for each additional Compat-managed skill
root you want to migrate. External skill paths configured through Compat, such
as a superpowers install, are also discovered from the Compat config's
`skills.paths` list.

To remove only lockfile-managed migrated state:

```sh
cargo run -p xtask -- sync-compat \
  --prune \
  --hya-config "$HOME/.config/hya/config.yaml" \
  --hya-skills-root "$HOME/.config/hya/skills"
```

The prune path removes only migration-owned MCP entries and migration-owned
skill symlinks. It must not delete unrelated user-authored hya config or
skills.

## Plugins

Plugins may be declared directly in config or discovered from
`<workdir>/.hya/plugins/**/plugin.toml`:

```yaml
plugins:
  memory:
    command: [python3, memory.py]
    timeout_ms: 500
    env:
      TOKEN: literal-token
  compat:
    kind: compat
```

Config entries support:

| Field | Meaning |
| --- | --- |
| `kind` | `rust`, `compat`, or `other`; default is `rust`. |
| `command` | Process command for stdio JSON-RPC. |
| `enabled` | Defaults to `true`; disabled entries are skipped. |
| `timeout_ms` | Optional request timeout. |
| `env` | Environment variables passed to the plugin process as configured. |

For `kind: compat` entries without `command`, hya uses the bundled Bun
adapter from `crates/hya-plugin-compat/adapter`. Set `BUN` to choose a Bun
binary or `HYA_COMPAT_ADAPTER_DIR` to point at an alternate adapter checkout.
If Bun is not available, that plugin is skipped.

The plugin host supports registered tools, command/message/text/chat hooks,
event notifications, permission hooks, shell/tool hooks, and workspace adapter
metadata.

## Formatter

The `formatter` key controls the formatter plane exposed through tools and the
Compat-compatible `/formatter` route:

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
3. `$HOME/.config/hya/prompts/*.md`
4. `<workdir>/.opencode/commands/*.md`
5. `<workdir>/.opencode/command/*.md`
6. `<workdir>/.hya/prompts/*.md`

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
built-in TUI profile, hya applies that profile before the turn starts. If
`model` is present, hya switches the submitted turn to that model.
