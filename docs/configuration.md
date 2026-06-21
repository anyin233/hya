# Configuration

yaca does not currently define its own provider configuration file. The CLI
builds model routes from opencode's config and falls back to an offline
development provider when no usable config is found.

## Config Discovery

The loader in [`../crates/yaca-cli/src/config.rs`](../crates/yaca-cli/src/config.rs)
checks:

1. `$XDG_CONFIG_HOME/opencode/opencode.json`
2. `$HOME/.config/opencode/opencode.json`

If neither file exists, or no supported providers are present, yaca uses
`DevProvider` from [`../crates/yaca-provider/src/dev.rs`](../crates/yaca-provider/src/dev.rs).

## Supported Provider Shapes

For each `provider.<id>` entry, yaca reads:

- `npm` to infer the provider protocol.
- `options.baseURL` as the upstream base URL.
- `options.apiKey` as a literal secret or a template.
- `models` keys as the model ids this route can serve.

Supported `npm` families:

| `npm` contains | yaca route |
| --- | --- |
| `openai` | OpenAI Chat Completions compatible route at `<baseURL>/chat/completions`. |
| `anthropic` | Anthropic Messages route at `<baseURL>/messages`. |

Providers without models, a base URL, or an API key are skipped.

## API Key Templates

`options.apiKey` supports:

```json
{ "apiKey": "literal-secret" }
```

```json
{ "apiKey": "{env:MY_PROVIDER_API_KEY}" }
```

```json
{ "apiKey": "{file:/absolute/path/to/key.txt}" }
```

Environment and file templates are resolved before building the provider router.
HTTP auth headers are marked sensitive and redirects are disabled in
[`HttpProvider`](../crates/yaca-provider/src/http.rs) so an auth header is not
forwarded across a redirect.

## Model Selection

The active model is selected in this order:

1. `--model <id>` CLI flag.
2. `YACA_MODEL` environment variable.
3. A configured model whose id contains `sonnet`.
4. The first configured model id.
5. `offline` when using the development provider.

Examples:

```sh
YACA_MODEL=claude-sonnet-4-6 yaca
yaca --model gpt-5.5 exec "summarize the architecture"
```

The selected model must be served by one of the configured provider routes. If
no provider reports capabilities for the model, the router returns an
`unknown provider for model` error.

## Offline Provider

When no usable live config exists, yaca creates a router with `DevProvider`. The
offline provider responds on every turn with a message that includes the latest
user prompt and says no live model is configured. This keeps the CLI, TUI, store,
server, and projection path testable without API keys.

## Custom Commands

yaca also reads opencode-style markdown commands for the TUI:

1. `$HOME/.config/opencode/commands/*.md`
2. `$HOME/.config/opencode/command/*.md`
3. `<workdir>/.opencode/commands/*.md`
4. `<workdir>/.opencode/command/*.md`

Project commands override user commands with the same file stem. The file stem
becomes the slash command name. Optional frontmatter fields are parsed:

```markdown
---
description: Create a component
agent: build
model: anthropic/claude-sonnet
---
Create $1 in $2.

All args: $ARGUMENTS
```

The first pass submits expanded command bodies as normal prompts; parsed
`agent` and `model` metadata are preserved for later agent/model routing.
