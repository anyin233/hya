# Configuration

hya reads its own YAML config from:

1. `$XDG_CONFIG_HOME/hya/config.yaml` (when that file exists)
2. `$HOME/.config/hya/config.yaml`

The file is parsed strictly as YAML via `serde_norway::from_str`
([`crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs)). JSON and
TOML are **not** accepted. Unknown top-level keys are ignored without warning —
there is no `deny_unknown_fields` — so a misspelled section such as `provider:`
instead of `providers:` is silently dropped. After editing, verify with
`hya-backend models`. A file that is empty or whitespace-only is treated exactly
like a missing file and hya runs offline.

If no configured or discovered live model row resolves, hya publishes exactly
`hya/offline`, served by `DevProvider` as a local echo model. It is removed when
any live row exists. The same config file also drives tools, MCP servers,
plugins, permissions, subagent limits, model categories, and formatter status.

## First-Run / Offline Behavior

On startup, hya tries to load `config.yaml` (see
[`../crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs) `load()`
and `config_path()`). `cargo build` only compiles the workspace and does not
write user config. When a `hya-backend` command starts and no file exists, hya
creates the config directory and writes a starter `config.yaml` before
resolving runtime config:

```yaml
default_model: hya/offline
providers: {}
mcp: {}
plugins: {}
permission:
  model: default
  rules: []
```

A missing or unusable config is **not an error**. Hya publishes the canonical
`hya/offline` row so the stack remains usable without credentials.

`load()` returns “no usable config” when there is no config file, the file is
empty, or it contains no provider, MCP, plugin, meaningful permission, or
`tools:` declaration. Provider declarations are not credential-gated: explicit
models build anonymous routes, and empty model lists make one optional-auth
catalog request during each startup.

**Meaningful permission:** a `permission:` block counts only if its `model`
differs from `default` **or** it has at least one rule
(`has_meaningful_permission`). The literal starter block
`permission: { model: default, rules: [] }` is treated as **absent**. A
permission-only config keeps hya’s config active only when that block is
meaningful by this rule.

**Independent re-reads:** `categories:` and `subagents:` are re-read by separate
loaders that reopen and reparse `config.yaml` independently of the main
`load()` (`load_categories`, `load_subagent_limits`). Both still take effect
even when hya is running on the offline provider because `load()` returned
`None`. A parse failure on those paths degrades silently to defaults — no error
is printed — so a malformed `categories:` / `subagents:` block looks like the
keys being ignored.

Normal startup never searches, opens, or imports Compat/OpenCode, Claude, Codex
CLI, Grok CLI, or another product's configuration. Offline import of supported
MCP servers and skills from a Compat config is available only through the dev
task `cargo run -p xtask -- sync-compat`; providers, models, and API keys are
not imported. Later startup reads only the Hya-owned result. First run creates
the starter Hya config without an import offer.

How to tell you are offline:

- Every catalog surface shows exactly `hya/offline` with offline metadata.
- Assistant replies echo the input and say that no live provider is available
  and a provider must be configured.

## Remembered Agent Models

Agent model selection covers primary Agents, ordinary subagents, and hidden
`title`, `summary`, and `compaction` Agents. Defaults and temporary choices have
separate owners: user configuration files, remembered defaults in the backend
Session database, and temporary overrides in the root Session event stream.
Attached and remote clients update the backend's state, not the client's files.

Model precedence is:

1. request-scoped model/category or explicit Workflow Stage route;
2. the active root Session's temporary selection for the target Agent;
3. the Agent's model in its owning user configuration file;
4. the authored Agent's direct model or model category;
5. an exact remembered model that still exists in the current provider catalog;
6. the existing Session/process default path.

Ordinary selection for an unconfigured Agent remembers its base model. For an
Agent with file or authored configuration, ordinary selection changes only the
active root Session and its descendants. Temporary overrides survive resuming
that Session but do not affect another root; already captured work remains
pinned. Clearing an override reveals the configured default again. Unavailable
retained temporary and remembered identities are not used.

Saving a **configured default** for an Agent creates or updates the owning
file; it does not erase a distinct Session override. Failed saves keep the
prior effective state and report an error. Old backends without the
`agentModelConfiguration` capability retain their original selection behavior.
(The interactive save flow shipped with the removed legacy TUI; a v1 rpc for
reading/writing per-Agent preferences has not been re-added yet — see below.)

Built-in Agents use the active Hya `config.yaml` (XDG path with the existing HOME
fallback). Bundle Agents use `agents/<encoded-bundle-id>/config.yml` beside it.
The immutable Bundle identity is encoded as one canonical percent-encoded
directory name: `hya/plan-impl-review` becomes `hya%2Fplan-impl-review`. Agents in
one WorkflowBundle share that file. Both files use the same model leaf:

```yaml
agents:
  hya-main:
    model: openai/gpt-4.1
```

Use the bundle Agent's stable id instead of `hya-main` in a bundle file. Saves
lock and reread the file, then atomically replace only the model leaf, preserving
unrelated settings, credentials, reasoning fields and file permissions.
Executable bundle sidecars receive `HYA_BUNDLE_CONFIG_DIR` and
`HYA_BUNDLE_CONFIG_FILE` for their own storage location; prepared bundle content
is never rewritten.

`GET /v1/bootstrap` advertises the effective catalog (agents, models,
providers) to frontends, and `PATCH /v1/sessions/{session}` switches a
session's `agent`/`model` fields. The dedicated per-Agent model-preference
HTTP control (`GET/PUT /tui/agent-models`) was part of the deleted Compat
surface; the durable preference files above are still read by runtime
composition (`PersistentAgentModelControl`), and a v1 rpc for reading/writing
per-Agent preferences has not been re-added yet.

The default durable database is
`$XDG_STATE_HOME/hya/sessions.db` (with the documented HOME fallback). An
explicit `--db <PATH>` has an independent preference set. In-memory execution
does not survive restart. Client presentation state such as recents, favorites,
and variants lives separately in `<state>/model.json`.

On a fresh client connection, the backend's effective Agent model is
authoritative; legacy Agent metadata is used only when no effective model row
is available. This keeps the displayed model and the next request aligned after
restarting both the client and the backend against the same database.

Non-interactive commands create the starter file without prompting and keep
machine-readable stdout clean. The only runtime config message they print is
when provider configuration resolution fails — hya logs to stderr and may
continue offline. Malformed Agent configuration or storage-read failures instead
prevent runtime readiness rather than silently publishing empty model defaults:

```text
hya: config error (...); using the offline provider
```

To leave offline mode, declare a provider with explicit models or an endpoint
that can return models. Credentials are optional (see [Providers](#providers)).

## Sample `config.yaml`

A copy-paste starting point covering a default model, a live provider, an MCP
server, and a plugin. Remove the parts you do not need; every top-level section
is optional.

```yaml
# ~/.config/hya/config.yaml  (or $XDG_CONFIG_HOME/hya/config.yaml)

# Row-backed process default. A stale value is ignored and the deterministic
# first resolved row is selected instead.
default_model: anthropic/claude-sonnet-4-6

# Optional: agent profile selected when a workdir does not specify one.
# Falls back to the built-in `build` agent when omitted.
default_agent: build

# Nested subagent caps (optional; defaults shown).
# Recursion depth is NOT configurable: it is hardcoded to two subagent
# layers (ADR-0015). A legacy `max_depth` key parses but is ignored.
subagents:
  max_concurrency: 100
  per_run_budget: 1024
  per_team_turn_budget: 1024
  per_team_message_budget: 1024

# Context management (optional; defaults shown). Thresholds and the token
# accounting they are measured with. Per-field HYA_COMPACTION_* and
# HYA_TOKEN_ACCOUNTING env vars override these. The five reduction
# mechanisms and the method_order semantics are documented in
# docs/compaction.md.
compaction:
  token_threshold: 100000      # used when the route advertises no window
  keep_recent: 6               # messages kept verbatim
  context_fraction: 0.75       # share of an advertised window
  reserve_tokens: 16384        # held back for the reply; tighter bound wins
  summary_max_tokens: 4096     # output cap for the summarizer call
  token_accounting: auto       # auto | provider | estimate
  # Order the five built-in reduction mechanisms (oh-my-pi names) are tried in
  # when a turn crosses the threshold. The walk stops at the first mechanism
  # that fits; an unavailable one (unsupported route, no summarizer) advances
  # to the next. A partial list is completed with the unmentioned names in
  # default order; an unknown name ignores the whole list.
  #   shake        evict stale tool outputs to artifact:// handles (no model)
  #   remote       provider-native compaction (/responses/compact)
  #   soft         structured LLM summary of the folded prefix
  #   snapcompact  local deterministic dense archive (no model call)
  #   handoff      LLM handoff document over the verbatim transcript
  method_order: [shake, remote, soft, snapcompact, handoff]

# Logical model categories → ordered provider/model failover lists.
categories:
  deep:
    - anthropic/claude-sonnet-4-6
    - gateway/gpt-5.6-sol

# Invocation policy. Selectors are Rust regular expressions and are evaluated
# in order. Use anchors when you need a full-name or full-command match.
permission:
  model: default                         # allow | default | strict | danger
  rules:
    - target: tool                       # tool | mcp | command
      selector: "^(read|grep)$"
      permission: Allow                  # Allow | Ask | Deny
    - target: mcp
      selector: "^mcp__github__"
      permission: Ask
    - target: command
      selector: "^git (status|diff)"
      permission: Allow

# Web search defaults to enabled, unauthenticated Exa when omitted.
tools:
  websearch:
    provider: exa                        # exa | parallel
    # endpoint: https://mcp.exa.ai/mcp
    # key: your-api-key
    enabled: true

# Each entry under `providers.<id>` becomes one HTTP route. The <id> is also the
# name used by `hya-backend login <id>` and shown as the provider in model refs.
providers:
  anthropic:
    kind: anthropic                      # openai-completion | openai-response | openai-codex | grok-build | anthropic | google
    base_url: https://api.anthropic.com/v1
    # Inline key is optional. Forms: literal, {env:VAR}, or {file:/path}.
    # A token saved via `hya-backend login anthropic <token>` takes precedence.
    api_key: "{env:ANTHROPIC_API_KEY}"
    models: [claude-sonnet-4-6]          # explicit list: normalized, no catalog request

# MCP servers. Tools are registered as mcp__<server>__<tool>.
# Stdio/local only — there is no url/remote transport key.
mcp:
  filesystem:
    command: [node, /path/to/server.js]  # argv array for the stdio server process
    env:
      TOKEN: "{env:MCP_TOKEN}"           # env values accept {env:}/{file:}
    timeout_ms: 1000                     # milliseconds; omit for 30s default
    # enabled: false                     # set to skip this server

# Plugins. Also discovered from $CWD/.hya/plugins/<name>/plugin.toml
# (backend process working directory at startup — not the session workdir;
# one directory deep — not recursive).
plugins:
  memory:
    command: [python3, memory.py]        # stdio JSON-RPC process
    timeout_ms: 500
    env:
      TOKEN: literal-token               # NOT templated — see Plugins
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
    kind: openai-response
    base_url: https://gateway.example/v1
    api_key: "{file:/run/secrets/gateway-key}"
    models:
      - id: gpt-5.6-sol
        reasoning:
          default: medium
          variants: [none, minimal, low, medium, high, xhigh, max]
  grok:
    kind: grok-build
    base_url: https://cli-chat-proxy.grok.com/v1
    # OAuth access token from `grok login` (JWT). Keep it in this config or via
    # `hya-backend login grok <token>` — hya does not read `~/.grok/auth.json`.
    api_key: "{env:GROK_OAUTH_TOKEN}"
    models: [grok-4.5]
  google:
    kind: google
    base_url: https://generativelanguage.googleapis.com
    api_key: literal-secret
    models: [gemini-2.0-flash]
```

Supported `kind` values:

| `kind` | Route |
| --- | --- |
| `openai`, `openai-compatible`, or `openai-completion` | OpenAI Chat Completions compatible route (`/chat/completions`). |
| `openai-response` | OpenAI Responses route (`/responses`). |
| `openai-codex` | ChatGPT Codex subscription Responses route (`/responses` on `chatgpt.com/backend-api/codex`). |
| `grok-build` | Grok Build Responses route (`/responses`). |
| `anthropic` | Anthropic Messages route. |
| `google` | Gemini route. |

`models` has two authoritative modes:

- A non-empty list is trimmed and exactly deduplicated. It is trusted without a
  model-list request and remains routable when no credential exists.
- An absent, empty, or blank-only list prefers `models.yml.cache` beside
  `config.yaml` and refreshes in the background. A cache miss still makes
  one bounded request during startup. Discovered rows persist in
  `models.yml.cache` and are preferred on the next load; they are never
  rewritten into `config.yaml`. Explicit `providers.*.models` still wins.
  Authentication headers are sent only when Hya has a credential.

### Provider retry

Transient route failures replay inside one shared budget per streamed
completion. A top-level `provider_retry:` block sets the default for every
route; a provider's `retry:` block overrides individual fields (unset fields
inherit the global value); `HYA_PROVIDER_RETRY_*` environment variables win
over both:

```yaml
provider_retry:
  max_attempts: 5        # total request attempts per completion (default 3)
  backoff_base_ms: 250   # exponential backoff seed (default 100)
  backoff_max_ms: 60000  # backoff / Retry-After ceiling (default 30000)

providers:
  flaky-gateway:
    kind: openai
    base_url: https://gw.example/v1
    api_key: "{env:GW_KEY}"
    models: [gpt-5.5]
    retry:
      max_attempts: 8    # inherits backoff fields from provider_retry
```

| Field | Env override | Default | Meaning |
| --- | --- | --- | --- |
| `max_attempts` | `HYA_PROVIDER_RETRY_MAX_ATTEMPTS` | `3` | Total request attempts per streamed completion, shared by pre-stream retries (transport, 429, 5xx) and the zero-event replay window. Clamped to at least 1. |
| `backoff_base_ms` | `HYA_PROVIDER_RETRY_BACKOFF_BASE_MS` | `100` | Exponential backoff seed; grows `2^attempt` with jitter (75–125%). |
| `backoff_max_ms` | `HYA_PROVIDER_RETRY_BACKOFF_MAX_MS` | `30000` | Ceiling for the exponential backoff and `Retry-After` waits (the latter is additionally hard-capped at 30 s). |

The budget covers both recovery layers: the pre-stream attempt loop, and the
zero-event replay window — when an established response dies before delivering
any event to the consumer (truncated body, connection reset, idle stall before
the first frame), the whole request is re-issued while budget remains. Once a
single event has been delivered the strict no-replay boundary applies and
errors surface exactly once.

Discovery uses the declared provider kind and base URL: OpenAI-compatible and
Responses use `/models`; Anthropic uses `/models` with bounded cursor pages;
Google uses `/models`, keeps `generateContent` rows, and strips `models/`;
Codex and Grok Build use their existing Hya catalog adapters and protocol
headers. Redirects are rejected; connect/page/batch deadlines are 3/8/10
seconds; each page is limited to 1 MiB, each provider to 8 pages and 2,000
models, and at most 4 providers run concurrently. There is no retry loop.

Provider startup status is non-secret and orthogonal: source is
`configured|discovered|none|offline`, auth is
`credentialed|unauthenticated|auth_required|auth_rejected|not_applicable`, and
result is `models|empty|unavailable|invalid|unsupported|offline`. These are
composition facts, not health or entitlement claims. A credentialless 401/403
is `auth_required`; a credentialed 401/403 is `auth_rejected`. Either produces
no remote row.

`grok-build` uses the Responses request shape and adds encrypted reasoning
content. Its fallback reasoning efforts are `low`, `medium`, and `high`,
defaulting to `high`. Grok streams must end with `response.completed` or
`response.incomplete`; `[DONE]` alone is not completion.

### Reasoning metadata

Models may use a string id or a detailed entry:

```yaml
models:
  - gpt-5.5
  - id: gpt-5.6-sol
    reasoning:
      default: medium
      variants: [none, minimal, low, medium, high, xhigh, max]
```

Accepted effort strings (case-insensitive after trim): `none`, `minimal`,
`low`, `medium`, `high`, `xhigh`, `max`. Compatibility aliases: `off` → `none`,
`med` → `medium`. Any other string is a config error.

An explicit non-`none` default must appear in `variants`. When `variants` is set
on a model, it **replaces** (does not extend) the provider-kind default menu.

**Per-kind default variant menus** (used when a model has no
`reasoning.variants` override), from
[`crates/hya-provider/src/http.rs`](../crates/hya-provider/src/http.rs):

| `kind` | Default variants |
| --- | --- |
| `anthropic` | `low`, `medium`, `high`, `max` |
| `openai` / `openai-compatible` / `openai-completion` | `minimal`, `low`, `medium`, `high`, `xhigh` |
| `openai-response`, `openai-codex` | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| `grok-build` | `low`, `medium`, `high` |
| `google` | `high`, `max` |

When `reasoning.default` is omitted, the effective default is the **highest**
effort in the resulting list (ordering Off &lt; Minimal &lt; Low &lt; Medium &lt;
High &lt; XHigh &lt; Max). Shipped default resolution uses
[`resolve_default_reasoning`](../crates/hya-provider/src/lib.rs) from
[`crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs), which always
passes `last_used: None`:

1. Explicit `reasoning.default` from config (must be advertised, else config error).
2. Otherwise the highest supported level among the route's advertised variants.

If the model advertises no reasoning at all, the result is `None` and no default
is shown. A route emits an empty variant list when `reasoning_request` is false.

The helper also accepts a `last_used` argument (kept when it is `none`/`off` or
present in the advertised variants), but **no production caller supplies it** —
only unit tests exercise that branch. hya does **not** remember a previously
selected effort across runs or UI picks.

**Provider budget / label mapping:**

| Effort | Anthropic thinking budget | Google `thinkingBudget` | OpenAI `reasoning_effort` label |
| --- | --- | --- | --- |
| `none` / `off` | none (disabled) | none | omitted |
| `minimal` | none (disabled) | none | `minimal` |
| `low` | 1024 | none | `low` |
| `medium` | 4096 | none | `medium` |
| `high` | 16000 | 16000 | `high` |
| `xhigh` | 24000 | 20000 | `xhigh` |
| `max` | 31999 | 32768 if model id contains both `2.5` and `pro`, else 24576 | `xhigh` |

Surprise: Google does **not** attach a thinking budget for off/minimal/low/medium.
Responses sends configured labels unchanged; Chat Completions omits `none` and
collapses both `xhigh` and `max` to the label `xhigh`.

### OAuth login (`openai-codex` and `grok-build`)

Interactive OAuth is implemented entirely in Rust:

```sh
# ChatGPT / Codex subscription (Codex default: device-code, print URL, no auto-open browser)
hya-backend oauth login --provider codex --type openai-codex
# same commands on the TypeScript launcher:
hya oauth login --provider codex --type openai-codex
# optional: open the verification URL, or use localhost PKCE instead of device-code
#   --browser
#   --loopback --browser

# xAI SuperGrok / Grok CLI (device-code flow)
hya-backend oauth login --provider grok --type grok-build --no-browser
hya oauth login --provider grok --type grok-build --no-browser

hya-backend oauth status
hya oauth status
```

On success hya:

1. Writes an OAuth credential bundle to the Hya auth directory (see
   [Auth Tokens](#auth-tokens)).
2. Optionally fetches a catalog only to improve login confirmation output.
3. Upserts the non-secret Hya provider declaration (`kind`, `base_url`) and
   preserves any existing `models` and inline `api_key` fields.

Login never writes fetched or guessed model IDs and never changes
`default_model`. A new provider gets `models: []`, so the next startup performs
the authoritative discovery request with the saved credential. Failed or empty
login-time preview does not create a fallback row; credentials and the empty
provider declaration are still saved. Model filtering and normalization happen
at startup through the same provider discovery adapters used by all other
catalog consumers.

**Provider id validation:** `--provider` ids containing `/`, `\`, `..`, or
whitespace are rejected, because the id becomes the `auth/<id>.yaml` filename.

#### Access-token refresh

`ensure_access_token` loads the saved credential on every stream and refreshes
first when the token is within a **5-minute (300s)** skew of its stated expiry. A
plain `type: api` credential is returned untouched. Refresh is guarded by a
process-wide mutex plus a re-read after acquiring the lock, so concurrent
sessions cannot both burn a rotated refresh token.

Two failure modes surface as `ProviderError::AuthExpired{provider, hint}`:

1. **NeedsLogin** — refresh token revoked/invalid (`invalid_grant`, or HTTP
   400/401 on the Grok token endpoint). Hint is the re-login command:

   ```text
   hya-backend oauth login --provider <name> --type <openai-codex|grok-build>
   ```

2. **Entitlement** — HTTP 403 from the Grok refresh endpoint means the account
   lacks subscription entitlement. The hint explains the API-key / upgrade path;
   **re-login will not fix it**.

Grok refresh requires a rotated refresh token when the response supplies one.

#### Codex OAuth endpoints

Useful for proxy/firewall allowlisting
([`openai_codex.rs`](../crates/hya-app/src/oauth/openai_codex.rs)):

| Constant | Value |
| --- | --- |
| `client_id` | `app_EMoamEEZ73f0CkXaXp7hrann` |
| Issuer / authorize / token | `https://auth.openai.com` |
| Device API base | `https://auth.openai.com/api/accounts` |
| Scope | `openid profile email offline_access` |

The ChatGPT account id sent as `chatgpt-account-id` is read from the `id_token`
(falling back to the access token) by decoding the JWT payload **without**
signature verification and reading
`https://api.openai.com/auth`.`chatgpt_account_id`.

Codex returns the device-flow `interval` as a JSON **string** rather than a
number; hya parses either form, floors it at 1 second, and defaults to 5 seconds
when absent.

Note: `client_version=0.144.0` is **pinned** and may need bumping if the models
endpoint starts rejecting it.

#### Grok OAuth endpoints

([`grok_build.rs`](../crates/hya-app/src/oauth/grok_build.rs)):

| Constant | Value |
| --- | --- |
| `client_id` | `b1a00492-073a-47ea-816f-4c329264a828` |
| Device / token | `https://auth.x.ai/oauth2/device/code`, `https://auth.x.ai/oauth2/token` |
| Scope | `openid profile email offline_access grok-cli:access api:access conversations:read conversations:write` |

Device-code polling: `authorization_pending` keeps polling; each `slow_down`
adds 5s to the interval capped at 30s; `access_denied` / `expired_token` fail
fast. When the token response omits `expires_in`, `expires_at` is derived from
the access token’s JWT `exp` claim.

#### OpenAI Codex (`kind: openai-codex`)

```yaml
providers:
  codex:
    kind: openai-codex
    base_url: https://chatgpt.com/backend-api/codex
    models: [gpt-5.3-codex]
```

Requests send `Authorization: Bearer <access_token>` and, when known,
`ChatGPT-Account-Id`. Do not point Codex OAuth tokens at `api.openai.com`.

#### Grok Build OAuth (`kind: grok-build`)

Credentials are **self-contained in hya config / auth** (`hya-backend oauth
login` or `hya-backend login`). hya never reads `~/.grok/auth.json`.

```yaml
providers:
  grok:
    kind: grok-build
    base_url: https://cli-chat-proxy.grok.com/v1
    models: [grok-4.5]
```

Every `grok-build` request uses CLI chat-proxy session headers:

- `Authorization: Bearer <token>`
- `X-XAI-Token-Auth: xai-grok-cli`
- `x-grok-client-version: <hya version>`
- `x-grok-client-identifier: grok-cli`
- `x-grok-model-override: <model id>`

You can still paste a bearer with `hya-backend login grok <token>` or an
inline `api_key`, but that path has no automatic refresh.

## Categories

Top-level `categories:` maps a logical category name to an ordered list of
concrete `provider/model` refs. The first entry is preferred; the rest form a
spawn-time failover chain (first candidate whose provider is configured wins).

```yaml
categories:
  deep:
    - anthropic/claude-sonnet-4-6
    - gateway/gpt-5.6-sol
  quick:
    - gateway/gpt-5.6-sol
```

Semantics ([`crates/hya-core/src/category.rs`](../crates/hya-core/src/category.rs),
[`config.rs`](../crates/hya-app/src/config.rs)):

- Empty or whitespace-only candidates are trimmed and dropped.
- A category whose list is entirely empty is dropped from the registry.
- Resolution picks the first candidate the router can actually serve; otherwise
  it falls back to the first candidate so failure surfaces as a real provider
  error instead of a silent misroute.
- There are **no** built-in categories (old `tier-cheap` / `strong` placeholders
  were removed).
- An unknown category fails to resolve and falls through the agent
  model-precedence chain to the global default model.

See [ADR-0004](adr/0004-model-category-resolution-and-precedence.md) for full
spawn/inline/bundle precedence. Categories are still loaded while offline (see
[First-Run / Offline Behavior](#first-run--offline-behavior)).

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
hya-backend oauth login --provider codex --type openai-codex
hya-backend auth list
hya-backend oauth status
hya-backend auth logout anthropic
```

### On-disk auth file schema

Directory: `$XDG_CONFIG_HOME/hya/auth` when `XDG_CONFIG_HOME` is set, otherwise
`$HOME/.config/hya/auth` ([`auth.rs`](../crates/hya-app/src/auth.rs)). Any saved
credential always beats an inline `providers.<id>.api_key`.

**Static API key:**

```yaml
type: api
token: sk-...
```

**OAuth bundle:**

```yaml
type: oauth
oauth_type: openai-codex   # or grok-build
access_token: ...
refresh_token: ...
expires_at: "2026-01-01T00:00:00Z"   # RFC3339 UTC
account_id: optional
id_token: optional
```

Writes are atomic: a temp `.<provider>.yaml.tmp` is created, chmodded to `0600`
on Unix, then renamed into place. If YAML deserialization yields no credential,
hya scrapes a bare `token: "..."` line and unquotes it as an API credential, so
a hand-written one-line file still works.

HTTP auth headers are marked sensitive and redirects are disabled so a secret is
not forwarded to another host.

## Model Selection

For a new session, an explicit `--model` or `HYA_MODEL` request is applied by
runtime resolution. Without that override, startup asks the immutable catalog
snapshot to use `default_model` from `config.yaml` only when it names a resolved
row (a bare model id is accepted only when unique). A stale, missing, or
ambiguous value is ignored and the first catalog row in deterministic
provider/model order becomes the default. With no live rows, that row is
`hya/offline`.

Examples:

```sh
HYA_MODEL=claude-sonnet-4-6 hya
hya-backend --model gpt-5.5 exec "summarize the architecture"
hya-backend models
hya-backend models gateway --verbose
```

The selected model must be served by one configured route. If no route reports
capabilities for the model, the router returns `unknown provider for model`.

### Model ref forms

A route serves a model ref addressed as:

| Form | Example |
| --- | --- |
| Bare `modelID` | `gpt-5.6-sol` |
| `providerID/modelID` (provider id must match the route) | `gateway/gpt-5.6-sol` |
| Either form with a non-empty `#variant` suffix | `gateway/gpt-5.6-sol#high` |

The `#variant` suffix is split off before matching and is **never** sent
upstream — it only selects the reasoning variant. An empty variant (trailing
`#`) is not treated as a suffix.

The `hya.v1` contract carries model references as this plain string form (for
example `CreateSession.model`). The former Compat HTTP surfaces also accepted a
model-ref object `{ providerID, modelID|id, variant? }`; that object form was
deleted with them and is not part of v1.

## Subagent Limits

Top-level `subagents:` caps nested and parallel subagent fan-out
([`crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs),
[`crates/hya-core/src/orchestrator.rs`](../crates/hya-core/src/orchestrator.rs)).
Every field is optional; omitted fields keep their default. Still applied while
offline (independent loader). See also
[ADR-0002](adr/0002-resident-actor-model-and-autonomous-main-agent.md).

```yaml
subagents:
  max_concurrency: 100
  per_run_budget: 1024
  per_team_turn_budget: 1024
  per_team_message_budget: 1024
```

Recursion depth is hardcoded to two subagent layers (ADR-0015) and is not a
config key; a legacy `max_depth` entry in the block parses but is ignored.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `max_concurrency` | usize | `100` | Ceiling on concurrently streaming general members (`1..=100`); excess members park rather than fail. |
| `per_run_budget` | u64 | `1024` | Maximum total members spawned under one top-level run. |
| `per_team_turn_budget` | u64 | `1024` | Total resident turns one team may run; tripping it **kills the team** (runaway re-wake backstop). |
| `per_team_message_budget` | u64 | `1024` | Total `MailSent` events one team may emit; tripping it **kills the team** (A↔B message-loop backstop). |

Environment overrides for these five keys win over the file (see
[Environment Variables](#environment-variables)). Unparseable env values fall
back to the config/default value.

## Web Search

Web search is enabled by default and uses Exa without authentication. Override
it under `tools.websearch`:

```yaml
tools:
  websearch:
    provider: exa # exa or parallel
    endpoint: https://mcp.exa.ai/mcp
    key: your-api-key
    enabled: true
```

`endpoint` and `key` are optional. Exa sends the key as the `exaApiKey` query
parameter; Parallel sends it as a bearer token. Set `enabled: false` to remove
the built-in `websearch` tool. When enabled, the tool is available to every
model provider. Provider, endpoint, key, and enabled come **only** from this
config block — there are no websearch-related environment variables.

## Permissions

`permission` controls registered tool, MCP, and shell-command invocations:

```yaml
permission:
  model: default
  rules:
    - target: tool
      selector: "^(read|grep)$"
      permission: Allow
    - target: mcp
      selector: "^mcp__github__"
      permission: Ask
    - target: command
      selector: "^git (status|diff)"
      permission: Deny
```

`model` (alias: `mode`) accepts lowercase `allow`, `default`, `strict`, or
`danger`. Rule `target` accepts lowercase `tool`, `mcp`, or `command`; rule
`permission` accepts `allow`/`Allow`, `ask`/`Ask`, or `deny`/`Deny`. Selectors
use Rust regular-expression search semantics, so `read` also matches
`read_file`; use `^read$` for an exact match. Invalid values or regular
expressions produce a config error and a strict permission fallback.

Rules are evaluated in file order:

| Model | Behavior |
| --- | --- |
| `allow` | Any matching `Deny` denies; otherwise allow. Does not prompt for tool, MCP, command, or external-directory checks. |
| `default` | The last matching rule wins. Without a match, local read-only and task tools allow; other tools, MCP calls, and commands ask. |
| `strict` | Any matching `Deny` denies; otherwise ask, except for an exact subject previously approved with Allow Always. |
| `danger` | Allow immediately, bypassing configured and legacy permission checks (including explicit Deny rules). |

The default local read-only set is `read`, `ls`, `glob`, `find`, `grep`, `lsp`,
`skill`, `list_agents`, `roster`, and `channels`; `task` also allows by default.
Network reads (`webfetch` and `websearch`), writes, plugins, MCP tools, and shell
commands ask by default under `default`/`strict`. Under `allow`, those resource
checks auto-approve unless a snapshot rule explicitly denies them.

Server mode forwards asks to the pending-interaction API for connected clients.
Headless `exec`, RPC, and goal modes reject unresolved asks.
`--yolo` replaces the effective model with `danger` before engine construction.

Omitting `permission` is equivalent to `model: default` with no rules. A
permission-only config remains active while hya uses the offline provider **only
when the block is meaningful** (non-default model or at least one rule) — see
[First-Run / Offline Behavior](#first-run--offline-behavior).

## Environment Variables

The tables below list environment variables the codebase reads, grouped by
layer. They are **not** a claim of exhaustive completeness across every binary
and plugin; each row cites its source.

Unset variables fall back to the documented default unless noted. Beyond these,
hya honors `HOME` and `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_STATE_HOME` /
`XDG_CACHE_HOME` for path derivation.

### Backend / runtime (`HYA_*`)

| Variable | Effect | Default | Source |
| --- | --- | --- | --- |
| `HYA_MODEL` | Active request model when `--model` is not passed. **Wins over** the row-backed startup default. Unknown overrides stay outside the catalog and fail through normal routing when used. | Configured `default_model` when it names a resolved row, otherwise the deterministic first live row, otherwise `hya/offline`. | `crates/hya-app/src/runtime.rs`, `crates/hya-app/src/config.rs` |
| `HYA_COMPACTION_THRESHOLD` | Estimated tokens that trigger context compaction, used when the route advertises no context window. Overrides `compaction.token_threshold`; **env wins**. Unparseable values ignored. | `100000` | `crates/hya-core/src/compaction.rs`, `crates/hya-app/src/config.rs` |
| `HYA_COMPACTION_KEEP_RECENT` | Most-recent messages kept verbatim during compaction. Overrides `compaction.keep_recent`; env wins. Unparseable values ignored. | `6` | same |
| `HYA_COMPACTION_CONTEXT_FRACTION` | Share of an advertised `max_context` window the transcript may occupy. With a window, the trip threshold is the **tighter** of `window * fraction` and `window - reserve_tokens`, floored at `1000`. Missing or zero window, or a fraction outside `(0.0, 1.0]`, falls back to `HYA_COMPACTION_THRESHOLD`. Overrides `compaction.context_fraction`; env wins. | `0.75` | same |
| `HYA_COMPACTION_RESERVE_TOKENS` | Tokens held back from the advertised window for the model's reply. Bounds the trigger independently of the fraction: a generous fraction on a large window can still leave less headroom than the reply needs. Overrides `compaction.reserve_tokens`; env wins. | `16384` | same |
| `HYA_COMPACTION_SUMMARY_MAX_TOKENS` | Output cap for the summarizer call that folds the transcript prefix. The structured section template does not fit a smaller cap, and a truncated summary loses its trailing sections — the ones describing what to do next. Overrides `compaction.summary_max_tokens`; env wins. | `4096` | same |
| `HYA_COMPACTION_METHOD_ORDER` | Comma-separated order of the five compaction mechanisms (`shake`, `remote`, `soft`, `snapcompact`, `handoff` — oh-my-pi `methodOrder` names). A partial list is completed with the unmentioned mechanisms in default order; an unknown name ignores the value. Overrides `compaction.method_order`; env wins. | `shake,remote,soft,snapcompact,handoff` | `crates/hya-core/src/compaction.rs`, `crates/hya-app/src/config.rs` |
| `HYA_TOKEN_ACCOUNTING` | How window occupancy is measured. `auto` believes a route's reported usage only while it advertises usage support and stays plausible against the local estimate (ratio within `[0.5, 2.0]`), otherwise estimates locally; `provider` always trusts reported usage; `estimate` always counts locally. Overrides `compaction.token_accounting`; env wins. Unrecognized values ignored. | `auto` | `crates/hya-core/src/tokens.rs`, `crates/hya-app/src/config.rs` |
| `HYA_SUBAGENT_MAX_CONCURRENCY` | Overrides `subagents.max_concurrency`. Env wins. | `100` | same |
| `HYA_SUBAGENT_BUDGET` | Overrides `subagents.per_run_budget` (env name drops `PER_RUN`). Env wins. | `1024` | same |
| `HYA_SUBAGENT_TURN_BUDGET` | Overrides `subagents.per_team_turn_budget`. Env wins. | `1024` | same |
| `HYA_SUBAGENT_MESSAGE_BUDGET` | Overrides `subagents.per_team_message_budget`. Env wins. | `1024` | same |
| `HYA_EVENT_BUS_CAPACITY` | Live EventBus broadcast ring capacity. Must parse as `usize` **> 0** or ignored. **Env-only** (no config.yaml key). Raising it trades memory for tolerance of slow SSE consumers. | `8192` (`DEFAULT_BUS_CAPACITY`) | `crates/hya-app/src/config.rs`, `crates/hya-core/src/bus.rs` |
| `HYA_DEFER_SIDEPLANES` | When deferred (default), MCP connect runs after the engine is built so the HTTP listener comes up without waiting on MCP handshakes — MCP tools may not be registered for the very first prompt. Set to `0`, `false`, `off`, or `no` (case-insensitive, trimmed) for await-MCP-before-listen. Any other value, empty, or unset means deferred. | deferred (on) | `crates/hya-app/src/runtime.rs` |
| `HYA_MCP_BACKGROUND_AFTER_MS` | Foreground budget in milliseconds for `mcp__` tool calls. A call still running past the budget moves to the background: the turn gets a `[backgrounded]` tool result immediately, the real result is delivered later as a steered `[background job …]` user prompt, and `hya-backend serve` runs the reclaim turn when the session is idle. Unset, `0`, or unparsable disables backgrounding (every call stays synchronous). | unset | `crates/hya-app/src/runtime.rs`, `crates/hya-core/src/engine/turn.rs` |
| `HYA_COMPAT_ADAPTER_DIR` | Path to an alternate Compat plugin adapter checkout (`kind: compat` plugins). | Resolution order: this env override, executable-adjacent `../lib/hya/compat-adapter`, then workspace `crates/hya-plugin-compat/adapter`. | `crates/hya-app/src/plugins.rs` |
| `HYA_BACKEND_BIN` | Binary under test for the `startup-bench` xtask; overrides the default `hya-backend serve` target. | workspace `target/{profile}` binary | `crates/xtask/src/startup_bench.rs` |
| `HYA_STARTUP_TRACE` | When `1` or `true` (case-insensitive; any other value off), `hya-backend serve` emits a newline-delimited JSON startup mark to stderr after the listen line: `{"hya_startup":true,"mark":"backend_listen","wall_ms":…,"detail":"<url>"}`. | off | `crates/hya-backend/src/serve.rs` |

### Compat adapter (`HYA_*` / `COMPAT_*`)

Read by the bundled Compat plugin adapter
([`crates/hya-plugin-compat/adapter`](../crates/hya-plugin-compat/adapter)):

| Variable | Effect | Default / notes | Source |
| --- | --- | --- | --- |
| `HYA_COMPAT_OPTIONS_JSON` | JSON blob with a `plugin: [spec \| [spec, options]]` array **appended** after discovered specs. Malformed JSON becomes an `INVALID_PARAMS` initialize error. | empty → no extra plugins | `loader/discovery.ts` |
| `HYA_DIRECTORY` | Adapter working directory. | `process.cwd()` | `initialize.ts` |
| `HYA_WORKTREE` | Stop boundary for the ancestor config walk. | same as directory | same |
| `HYA_SERVER_URL` | `serverUrl` handed to plugins. | `http://127.0.0.1:0` | same |
| `HYA_PROJECT_ID` | Compat project id. | worktree path | `client_adapter.ts` |
| `COMPAT_CONFIG` | Explicit Compat config file path. | unset | `initialize.ts` |
| `COMPAT_CONFIG_DIR` | Extra config directory. | unset | same |
| `COMPAT_CONFIG_CONTENT` | Inline JSON config. | unset | same |
| `COMPAT_DISABLE_PROJECT_CONFIG` | Skip the project-config ancestor walk. | off | same |
| `COMPAT_PURE` | When `true` or `1`, load **zero** plugins (escape hatch when a plugin breaks startup). | off | same |

### Related non-`HYA_` variables

| Variable | Effect | Source |
| --- | --- | --- |
| `BUN` | Bun binary used to run the bundled Compat adapter. | `crates/hya-app/src/plugins.rs` |
| `SHELL` | Shell program for PTY sessions on the v1 PTY routes; also listed among shell candidates. Defaults to `/bin/sh` when **unset**. A variable that is set but empty is **not** replaced — PTY create may receive an empty command. | `crates/hya-server/src/support/pty_shell.rs` |
| `COMPAT_REPO_CLONE_GITHUB_BASE_URL` | Overrides the GitHub base URL when cloning reference repositories (Enterprise / internal mirror). Trailing slashes trimmed. Default remote is `https://github.com/<path>.git`. Store under `$XDG_DATA_HOME/compat/repos` (else `~/.local/share/compat/repos`). | `crates/hya-server/src/support/reference_repository.rs` |
| `COMPAT_TERMINAL` | **Output only:** set to `1` in every PTY child environment so programs can detect the hya terminal. hya never reads it. | `crates/hya-server/src/support/pty_state.rs` |

## MCP Servers

hya supports **stdio/local** and **remote HTTP** MCP servers. `mcp.<name>.command`
is an argv array for a local stdio server; `mcp.<name>.url` connects to a remote
server over **Streamable HTTP** (the 2025-06-18 default) or, with
`transport: sse`, the classic HTTP+SSE transport. When importing a
Compat/OpenCode config, entries with a non-empty `url` are imported as remote
`url:` servers; entries with `type: local` and a non-empty command are imported
as stdio servers; anything else is skipped.

```yaml
mcp:
  filesystem:
    command: [node, /path/to/server.js]
    env:
      TOKEN: "{env:MCP_TOKEN}"   # {env:}/{file:} templating applies here
    timeout_ms: 1000             # milliseconds; omit → 30s per subsequent call
  remote:
    url: https://mcp.example.com/mcp   # Streamable HTTP
  legacy-remote:
    url: https://old.example.com/sse
    transport: sse                      # classic HTTP+SSE (2024-11-05)
  disabled-example:
    enabled: false
    command: [node, server.js]
```

`timeout_ms` is milliseconds. When omitted, the per-call timeout is **30 s**
(`DEFAULT_CALL_TIMEOUT` in [`crates/hya-mcp/src/client.rs`](../crates/hya-mcp/src/client.rs)).
When both `url` and `command` are set, `url` wins. Unknown `transport` values
fail the connection; the v1 `AddMcpServer` route with a `Url` transport always
selects Streamable HTTP.

### Connection handshake

When connecting an enabled server, hya:

1. Opens the transport: spawns `command` over stdio, POSTs to `url`
   (Streamable HTTP), or opens the SSE event stream and its POST endpoint
   (classic SSE). For Streamable HTTP the `Mcp-Session-Id` response header of
   `initialize` is replayed on every later request; stateless servers that
   never issue one work unchanged.
2. Sends `initialize` with `protocolVersion: "2025-06-18"` and `clientInfo`
   naming `hya`, under a **5-second** initialize timeout.
3. Sends the required `notifications/initialized` notification.
4. Calls `tools/list` (uses `timeout_ms` / 30s default).
5. Best-effort `resources/list` — a failing resources list still leaves the
   server **Connected** with zero resources.

Only steps 1–4 are mandatory for a successful connection.

### Long calls: auto-background and reclaim

MCP tools can be slow. With `HYA_MCP_BACKGROUND_AFTER_MS` set, any `mcp__` tool
call still running after the budget moves to the background instead of blocking
the turn:

1. The turn immediately receives a synthetic tool result marked
   `[backgrounded]` (metadata `{"backgrounded": true, "job": "mcpbg-N"}`), so
   the model knows the call continues out of band and can do other work.
2. When the call settles, the real outcome is recorded durably on the same
   tool part (`ToolResult` with metadata `background_result`, or `ToolError`
   with value `background_failed` on failure) and a steered user prompt
   `[background job mcpbg-N completed|failed: <tool>] … Reclaim this result`
   is appended to the session carrying the result text.
3. `hya-backend serve` watches for that marker: if the session is idle it
   starts a follow-up turn right away so the agent reclaims the result; a busy
   session simply sees the prompt on its next round. In `exec`/goal mode the
   prompt stays durable for the next turn.

Cancellation is honored: a backgrounded call cancelled with its session never
invents a result. Without the variable set, every MCP call stays synchronous
with only `timeout_ms` bounding it.

Enabled servers are prepared during runtime composition. Their tools keep the
external name `mcp__<server>__<tool>` and use the existing permission plane.
`GET /v1/mcp` (Mcp `GetMcpStatus`) composes connected, disabled, and failed
status from the current desired revision, observed handshake result, and
effective runtime generation. The v1 MCP control routes
(`POST /v1/mcp`, `POST /v1/mcp/{name}/connect`, `POST /v1/mcp/{name}/disconnect`)
update that same in-process desired state. A complete successful observation is
published atomically for the next turn; a failed handshake or name collision
leaves the prior effective view unchanged, and disconnect removes the source for
the next turn while an already-running turn retains its old binding. These
routes do not durably rewrite `config.yaml`.

With default `HYA_DEFER_SIDEPLANES`, the HTTP listener can come up before MCP
handshakes finish, so `GET /v1/mcp` may briefly report servers as not yet
connected right after startup. See [Environment Variables](#environment-variables) and
[`docs/testing/process-e2e.md`](testing/process-e2e.md).

### Runnable dynamic MCP control example

The following local fixture implements only the MCP calls needed by this
example. Save it as `/tmp/hya-mcp-ping.py`:

```python
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    if request["method"] == "initialize":
        result = {"capabilities": {}}
    elif request["method"] == "tools/list":
        result = {
            "tools": [
                {
                    "name": "ping",
                    "description": "Return pong",
                    "inputSchema": {"type": "object"},
                }
            ]
        }
    else:
        result = {"content": {"text": "pong"}, "isError": False}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
```

The equivalent persistent startup configuration is:

```yaml
mcp:
  live-demo:
    command: [python3, /tmp/hya-mcp-ping.py]
    timeout_ms: 1000
    enabled: false
```

Start the server, then exercise the actual v1 MCP control routes:

```sh
cargo run -p hya-backend -- serve --bind 127.0.0.1:8080 --db /tmp/hya-mcp-demo.db

# Status: initially absent unless it came from config.yaml.
curl -sS http://127.0.0.1:8080/v1/mcp

# Add to in-process desired state and connect. The v1 schema takes a
# `command` transport ({command, args, env}) and an optional `enabled`.
curl -sS -X POST http://127.0.0.1:8080/v1/mcp \
  -H 'content-type: application/json' \
  --data '{"name":"live-demo","command":{"command":"python3","args":["/tmp/hya-mcp-ping.py"]},"env":{},"enabled":true}'

# Observe desired/observed/effective status.
curl -sS http://127.0.0.1:8080/v1/mcp

# Disable and atomically remove its tools from the next TurnBinding.
curl -sS -X POST http://127.0.0.1:8080/v1/mcp/live-demo/disconnect
curl -sS http://127.0.0.1:8080/v1/mcp

# Re-enable the retained in-process desired entry.
curl -sS -X POST http://127.0.0.1:8080/v1/mcp/live-demo/connect
```

`disconnect` is the current remove-from-effective-view operation. There is no
general MCP config-delete route, and dynamic changes are not written back to
`config.yaml`.

### Compat migration into hya

Compat provider/model and local MCP configuration enters Hya only through the
explicit import command:

```sh
hya --import compat
```

The discovered Compat config is parsed as **strict JSON first**; if that fails,
`//` and `/* */` comments and trailing commas are stripped and it is re-parsed as
JSONC. This applies to any candidate filename, so a commented `opencode.json`
also imports.

The explicit import supports Compat provider/model config and local stdio MCP
entries. It replaces `default_model` and `providers` in `config.yaml`, merges
imported MCP servers by name, and preserves Hya-only MCP entries plus non-model
sections such as `plugins` and `default_agent`. Compat `type: "local"`,
`command`, `environment`, `enabled`, and `timeout` map to Hya `command`, `env`,
`enabled`, and `timeout_ms`. Remote/OAuth MCP entries are skipped and counted;
skills are not imported. Automatic startup never reads these foreign paths.

**Provider `kind` inference:** hya lowercases the Compat provider id plus its npm
package and display name and guesses `kind` — containing `anthropic` →
`anthropic`; containing `google` or `gemini` → `google`; anything else →
`openai-compatible`. Review and fix `kind` by hand after import for providers
that need `openai-response`, `openai-codex`, or `grok-build`.

**Filtering:** disabled providers are skipped, as is any provider without a
`base_url` or without at least one model; when the Compat default model belongs
to a provider, its id is folded into that provider’s model list.

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
- Compat provider/model sections are handled by explicit `hya --import compat`,
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
**`$CWD/.hya/plugins/<name>/plugin.toml`** — the backend process working
directory at startup (`plugins::plugins_dir()`), **not** the per-session
workdir (**one directory deep** — nested `plugin.toml` files are never found).
The two coincide when the launcher starts the backend with
`.current_dir(project)`; a bare `hya-backend serve` from another directory only
scans that process CWD:

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
| `timeout_ms` | Optional per-call timeout in **milliseconds**. When omitted: **30 s**. Fixed non-configurable timeouts: initialize **5 s**, shutdown **1 s**. |
| `env` | Environment variables passed **verbatim** to the child. The `{env:VAR}` / `{file:/path}` secret templating supported by `providers.<id>.api_key` and `mcp.<name>.env` is **not** applied here. Export the variable in the parent shell instead. |

### Directory manifests

Layout: `$CWD/.hya/plugins/<name>/plugin.toml` (process CWD at compose time —
see above) scanned from each **immediate** subdirectory of `.hya/plugins` only
([`crates/hya-app/src/plugins.rs`](../crates/hya-app/src/plugins.rs)
`plugins_dir` / `scan_manifests`). Missing or **unreadable** `plugin.toml` is
skipped **silently** (`read_to_string` failure → `continue` with no log). Only
an **unparseable** file prints
`hya: skipping plugin manifest <path> (<error>)` on stderr. Neither case fails
startup.

Example:

```toml
id = "memory"
kind = "rust"           # rust | compat (alias: opencode) | other; default rust
command = ["python3", "memory.py"]
enabled = true
timeout_ms = 500

[[hooks]]
name = "tool.execute.before"
posture = "safe"        # safe | open

[[hooks]]
name = "event"
```

| Field | Meaning |
| --- | --- |
| `id` | Required; must match the plugin’s handshake id. |
| `kind` | `rust` (default), `compat` (alias `opencode`), or `other`. |
| `command` | Required argv array. |
| `enabled` | Default `true`. |
| `timeout_ms` | Optional per-call override (ms). |
| `[[hooks]]` | Repeated tables: `name` plus optional `posture` (`safe` \| `open`). |

An **unknown** hook name is warned about and dropped; the manifest still loads
and the plugin runs without that hook. Manifest posture entries act as posture
overrides and survive the config/manifest merge; YAML config entries carry no
posture overrides.

### Config-over-manifest merge

From [`hya_plugin::config::merge`](../crates/hya-plugin/src/config.rs):

1. Config entries are emitted **first** (skipping any with `enabled: false`).
2. Manifests are appended only if their id was **not** already claimed by a
   config entry **and** the manifest itself is enabled.

Consequences: config always beats a same-id manifest; config `plugins` is a
`BTreeMap`, so config-declared plugins fold in **lexicographic plugin-id order**
(not YAML source order); setting `enabled: false` in config does **not** re-open
the id for a manifest to claim — the plugin is simply absent.

For `kind: compat` entries without `command`, hya resolves the adapter in this
order: (1) `HYA_COMPAT_ADAPTER_DIR` when set, (2) the installed adapter adjacent
to the executable at `../lib/hya/compat-adapter`, and (3) the workspace adapter
at `crates/hya-plugin-compat/adapter`. Set `BUN` to choose the Bun binary. If
Bun is not available, that plugin is skipped.

### Hook name vocabulary

A plugin may declare these hook names in its initialize handshake
([`crates/hya-plugin/src/messages.rs`](../crates/hya-plugin/src/messages.rs)).
Default posture when the plugin omits one: **Safe** for `permission.ask` and
`tool.execute.before`; **Open** for all others.

| Hook name | Default posture |
| --- | --- |
| `event` | Open |
| `command.execute.before` | Open |
| `experimental.text.complete` | Open |
| `message.user.before` | Open |
| `chat.params` | Open |
| `tool.execute.before` | Safe |
| `tool.execute.after` | Open |
| `permission.ask` | Safe |

These three names also parse and may appear in `plugin.toml` / initialize, but
the host **never dispatches** them (dead hooks — see
[Plugin protocol](plugin-protocol.md)):

| Hook name | Default posture | Status |
| --- | --- | --- |
| `goal.evaluate` | Open | Registered only; no dispatcher arm |
| `loop.verifier` | Open | Registered only; no dispatcher arm |
| `loop.planner` | Open | Registered only; no dispatcher arm |

**AgentBundle sidecars** may select only the three bundle-legal IDs: `event`,
`tool.execute.before`, and `tool.execute.after`. The wider list applies to
config-declared (and directory-manifest) plugins only.

### `chat_params` hook

Each plugin declaring the ChatParams hook can rewrite the outgoing completion
request before dispatch. The wire form exposes `model`, `system`, `messages`,
`tools`, `temperature`, `max_output_tokens`, `reasoning`, and `headers`
([`dispatcher.rs`](../crates/hya-plugin/src/dispatcher.rs)). `headers` become
per-request extra HTTP headers merged over the route’s auth headers. A
plugin-supplied `reasoning` string that fails to parse as a `ReasoningEffort`
leaves the **original** effort in place rather than clearing it.

The plugin host also supports registered tools, command/message/text/chat hooks,
event notifications, permission hooks, shell/tool hooks, and workspace adapter
metadata.

Plugin tools from the startup handshake are published through the same
immutable runtime registry as builtins and MCP tools. The configured plugin ID
must match the handshake ID. If a crashed plugin respawns, hya compares a
deterministic encoding of the complete initialize declaration (plugin metadata,
tools, hooks including command/permission hooks, and workspace adapters). A
changed declaration closes the replacement process and future calls fail
closed. There is no plugin watching, hot add/remove, or plugin reload command;
existing hook and `PermissionPlane` behavior is unchanged.

## Formatter

The `formatter` key controls the formatter plane exposed through tools; the
v1 bootstrap snapshot advertises availability as the `formatterAvailable` flag
(the former Compat `/formatter` status route is deleted). It is untagged:
either a bool or a map.

When the key is **absent**, the default is `false` (`FormatterConfig::Disabled`).
`true` enables the built-in formatter set (`FormatterConfig::Builtins`). A
**mapping** is `FormatterConfig::Custom`: it **merges** your entries over the
builtin set — it does **not** replace the builtins. Writing
`formatter: { treefmt: { … } }` keeps every available builtin **and** adds
`treefmt`. To stop a builtin, set `disabled: true` on that name (see
[`formatter_definition.rs`](../crates/hya-tool/src/formatter_definition.rs)).

```yaml
formatter: true
```

```yaml
formatter:
  treefmt:
    command: [treefmt, "$FILE"]
    extensions: [.nix]
  gofmt:
    disabled: true
```

Custom entries support `disabled`, `command`, `environment`, and `extensions`.
`$FILE` in `command` is the placeholder for the file being formatted. For a
known builtin name, a non-disabled map entry **merges** into that builtin
(override extensions/command/env); an unknown name is **appended** as a new
formatter (requires `command` / `extensions` as needed).

**Python pair:** the bulk dual-disable uses
`entries.get("ruff").or_else(|| entries.get("uv")).is_some_and(|e| e.disabled)`
— i.e. it inspects **`ruff` if that key is present**, otherwise **`uv`**. When
that chosen entry has `disabled: true`, **both** `ruff` and `uv` are removed
from the active set. If `ruff` is present and **not** disabled, a separate
`uv: { disabled: true }` only removes `uv` (ruff keeps formatting `.py` /
`.pyi`).

The formatter block is parsed **independently** of the rest of `config.yaml`. A
parse error there disables only formatting and prints on stderr:

```text
hya: formatter config error (...); formatter status disabled
```

It does not abort startup and does not push hya offline. The formatter runs
after successful `write`, `edit`, and `apply_patch` tool operations when a
matching definition is available (binary found and any probe succeeds). Several
builtins claim the same extensions (for example `.ts`); whichever enabled
definition matches first for that path runs. Binaries that are not on `PATH`
(or fail their probe) are skipped silently.

### Built-in formatter set (`formatter: true`)

Twenty-six builtins are defined in
[`formatter_catalog.rs`](../crates/hya-tool/src/formatter_catalog.rs). Only
entries whose availability probe succeeds actually run. Argv when enabled
comes from [`formatter_command.rs`](../crates/hya-tool/src/formatter_command.rs)
(`$FILE` = path being formatted).

| Name | Extensions | Typical argv (when enabled) | Availability notes |
| --- | --- | --- | --- |
| `gofmt` | `.go` | `gofmt -w $FILE` | `gofmt` on PATH |
| `mix` | `.ex` `.exs` `.eex` `.heex` `.leex` `.neex` `.sface` | `mix format $FILE` | `mix` on PATH |
| `prettier` | `.js` `.jsx` `.mjs` `.cjs` `.ts` `.tsx` `.mts` `.cts` `.html` `.htm` `.css` `.scss` `.sass` `.less` `.vue` `.svelte` `.json` `.jsonc` `.yaml` `.yml` `.toml` `.xml` `.md` `.mdx` `.graphql` `.gql` | `prettier --write $FILE` | `package.json` mentions `prettier`; binary on PATH |
| `oxfmt` | `.js` `.jsx` `.mjs` `.cjs` `.ts` `.tsx` `.mts` `.cts` | — | Catalog entry only today: builtin probe always returns disabled (`CheckKind::Oxfmt` → no argv) unless you override with a custom `command` |
| `biome` | same broad web set as prettier | `biome format --write $FILE` | `biome.json` or `biome.jsonc` found upward; binary on PATH |
| `zig` | `.zig` `.zon` | `zig fmt $FILE` | `zig` on PATH |
| `clang-format` | `.c` `.cc` `.cpp` `.cxx` `.c++` `.h` `.hh` `.hpp` `.hxx` `.h++` `.ino` `.C` `.H` | `clang-format -i $FILE` | `.clang-format` found upward; binary on PATH |
| `ktlint` | `.kt` `.kts` | `ktlint -F $FILE` | `ktlint` on PATH |
| `ruff` | `.py` `.pyi` | `ruff format $FILE` | `ruff` on PATH **and** ruff config/dependency signal (`[tool.ruff]`, `ruff.toml` / `.ruff.toml`, or `ruff` mentioned in requirements/pyproject/Pipfile) |
| `air` | `.R` | `air format $FILE` | `air` on PATH and `air --help` first line mentions R language formatter |
| `uv` | `.py` `.pyi` | `uv format -- $FILE` | Only when ruff is **not** enabled for the workdir; `uv` on PATH and `uv format --help` succeeds |
| `rubocop` | `.rb` `.rake` `.gemspec` `.ru` | `rubocop --autocorrect $FILE` | `rubocop` on PATH |
| `standardrb` | `.rb` `.rake` `.gemspec` `.ru` | `standardrb --fix $FILE` | `standardrb` on PATH |
| `htmlbeautifier` | `.erb` `.html.erb` | `htmlbeautifier $FILE` | binary on PATH |
| `dart` | `.dart` | `dart format $FILE` | `dart` on PATH |
| `ocamlformat` | `.ml` `.mli` | `ocamlformat -i $FILE` | `.ocamlformat` found upward; binary on PATH |
| `terraform` | `.tf` `.tfvars` | `terraform fmt $FILE` | `terraform` on PATH |
| `latexindent` | `.tex` | `latexindent -w -s $FILE` | `latexindent` on PATH |
| `gleam` | `.gleam` | `gleam format $FILE` | `gleam` on PATH |
| `shfmt` | `.sh` `.bash` | `shfmt -w $FILE` | `shfmt` on PATH |
| `nixfmt` | `.nix` | `nixfmt $FILE` | `nixfmt` on PATH |
| `rustfmt` | `.rs` | `rustfmt $FILE` | `rustfmt` on PATH |
| `pint` | `.php` | `./vendor/bin/pint $FILE` | `composer.json` mentions `laravel/pint` |
| `ormolu` | `.hs` | `ormolu -i $FILE` | `ormolu` on PATH |
| `cljfmt` | `.clj` `.cljs` `.cljc` `.edn` | `cljfmt fix --quiet $FILE` | `cljfmt` on PATH |
| `dfmt` | `.d` | `dfmt -i $FILE` | `dfmt` on PATH |

Disable an unwanted rewrite with a map entry, for example
`prettier: { disabled: true }` or `rustfmt: { disabled: true }`.

## Language Servers

The optional `lsp` key in `config.yaml` configures app-owned stdio language
servers. Servers start lazily for `lsp` operations, code-symbol queries, or
post-edit diagnostics. hya does not download or install language servers.

With the key absent or `lsp: true`, hya detects installed TypeScript
(`typescript-language-server --stdio`), Rust (`rust-analyzer`), Python
(`pyright-langserver --stdio`), Bash (`bash-language-server start`), Go (`gopls`),
and C/C++ (`clangd`) executables on PATH. `lsp: false` disables the plane.
A map overrides named defaults or adds custom servers:

```yaml
lsp:
  typescript:
    command: [/opt/typescript/bin/typescript-language-server, --stdio]
  rust:
    disabled: true
  custom:
    command: [/opt/example-language-server, --stdio]
    extensions: [.example]
    root_markers: [example.project, .git]
    environment: {}
    initialization_options: {}
    settings: {}
```

Named builtin overrides retain default extensions and root markers even when
the default command is not on PATH. Custom servers require `command` and
`extensions`. Configuration changes take effect after restarting the backend.
Servers are shared per language/workspace root; requests use advertised server
capabilities. Connection starts and failures refresh the LSP plane's internal
status (the former Compat `/lsp` status route is deleted; the v1 surface does
not yet expose an LSP status rpc).

Write/Edit/Patch diagnostics are limited to the requesting workdir and explicitly
authorized target files. Versioned publications or pull diagnostics track the
updated document; stale versions are rejected. Servers publishing without
versions have no completion signal, so hya collects their notifications for a
two-second **best-effort** window, including clear-then-delayed updates. No
publication within that window is reported as unavailable analysis, not a clean
bill of health. Transport errors, interrupted framed writes, and shutdown close
owned server processes; Unix teardown also terminates their process groups.

## Project Config (`opencode.json`)

At runtime hya reads, in this order:

1. `{workdir}/opencode.json`
2. `{workdir}/opencode.jsonc`
3. `{workdir}/.opencode/opencode.json`
4. `{workdir}/.opencode/opencode.jsonc`

A **later** file that sets a key overrides an earlier one
([`bound_agent_metadata.rs`](../crates/hya-server/src/support/bound_agent_metadata.rs)).
Only **`default_agent`** is honoured for agent selection — inline `agent`,
`permission`, `model`, and `options` fields present in an OpenCode project
config are deliberately **not** read. Unreadable or invalid files are skipped
silently with no error.

```json
{
  "default_agent": "build"
}
```

The same four paths may also declare inline slash commands (see
[Custom Commands](#custom-commands)).

## Custom Commands

Built-in slash commands are served by the backend command catalog over
`GET /v1/commands` (see [CLI Reference](cli.md#backend-command-catalog)). This
section covers **user-defined** prompt commands.

### Disk markdown commands

hya scans exactly two project-local roots
([`command_sources.rs`](../crates/hya-server/src/support/command_sources.rs)
`disk_commands`):

1. `<workdir>/.opencode/command/**/*.md`
2. `<workdir>/.opencode/commands/**/*.md`

Files are collected **recursively** and sorted by path. The slash-command name is
the path **relative to the discovery root**, with path segments joined by `/` and
the `.md` suffix stripped — e.g. `.opencode/command/git/commit.md` becomes
`/git/commit`, not `/commit`. There is **no** user/home tier and no
`.hya/prompts` path.

Optional YAML frontmatter:

| Field | Meaning |
| --- | --- |
| `description` | Shown in the command list / `GET /v1/commands` catalog. |
| `agent` | Optional string stored on `CommandInfo` and exposed in the command catalog / bootstrap summary only. **No runtime consumer** switches the session agent from this field; the turn uses the session's current agent. |
| `model` | Optional string stored and listed the same way as `agent`. Disk frontmatter does not switch the turn model. |
| `subtask` | Optional boolean parsed into the command wire payload. **No runtime consumer** currently reads it (frontends and the engine do not open a child session from this flag). |

```markdown
---
description: Create a component
agent: build
model: claude-sonnet-4-6
subtask: true
---
Create $1 in $2.

All args: $ARGUMENTS
```

`$1`, `$2`, … and `$ARGUMENTS` inside the body become numbered hint slots.
Expanded command bodies are submitted as normal prompts under the **session's
current** agent. Frontmatter `agent` / `model` / `subtask` do not change that
path. `CommandRequest` carries `command`, `arguments`, optional `text`, and
optional `model` / `variant`; when `model` is set, the command path calls
`model_ref()` and `switch_model` before the turn.

### Inline config commands

The same four project config paths may carry **both** a singular `command` map
and a plural `commands` map; the two are read and concatenated. Each entry is
keyed by command name:

| Field | Required | Meaning |
| --- | --- | --- |
| `template` | yes | Prompt body (`$1` / `$ARGUMENTS` hint slots apply). |
| `description` | no | List description. |
| `agent` | no | Listed in the command catalog only; **not** applied as a turn agent override (same as disk frontmatter). |
| `model` | no | Listed in the command catalog only; **not** applied as a turn model override. |
| `subtask` | no | Optional boolean on the command catalog; **not** used to spawn a child session today. |

These are upserted over the backend built-ins, so an entry named `review`
replaces the built-in `/review`.

```json
{
  "commands": {
    "review": {
      "template": "Review $ARGUMENTS with a focus on correctness.",
      "description": "Code review pass",
      "agent": "build",
      "subtask": false
    }
  }
}
```

## Skills

Skill discovery (ten-directory first-wins search path), `SKILL.md` frontmatter
(`name`, `description`, `allowed-tools`, `model`, `disable`, `license`), silent
skip rules, and built-in fallback skills are documented in
[Skills](skills.md).

## Project Context (`AGENTS.md`)

hya canonicalizes the workdir and walks **upward** toward the filesystem root
collecting every `AGENTS.md` it finds, **stopping** once it has processed
`$HOME` (files above the home directory are never read). The list is then
reversed so the outermost/parent `AGENTS.md` appears first in the system prompt
and the workdir-local one last
([`crates/hya-core/src/prompt.rs`](../crates/hya-core/src/prompt.rs)). Unreadable
or missing files are skipped silently. This is the sole discovery
implementation — callers re-export it rather than reimplement walk order.

## Project references (`references` / `reference`)

Project **references** are external directories the agent may use (local paths or
git clones). They power `@` alias autocomplete, turn-scoped
`ExternalDirectory` allow rules, and optional system-prompt guidance. There is
**no** `config.yaml` key and **no** on-disk file for this map: the only way to
declare them is the process-local runtime config bag —

- `PATCH /v1/config` (scope with the `directory` body field or
  `x-hya-directory` header)
- bag key: `references` **or** `reference` (object of alias → entry)

`PATCH /v1/config` **deep-merges** objects and replaces non-object leaves, so
patching `references` replaces that whole map. State is lost on process
restart. Source:
[`reference_entries.rs`](../crates/hya-server/src/support/reference_entries.rs),
[`reference.rs`](../crates/hya-server/src/support/reference.rs).

### Entry shapes

| Form | Meaning |
| --- | --- |
| string starting with `.`, `/`, or `~` | Local path (resolved against the session workdir; `~/…` uses `$HOME`) |
| any other string | Git repository shorthand (background-cloned; see cache root below) |
| `{ "path", "description"?, "hidden"? }` | Local path object |
| `{ "repository", "branch"?, "description"?, "hidden"? }` | Git object (`branch` must pass `valid_branch` or the entry is dropped) |

### Alias rules

Aliases that are empty or contain `/`, whitespace, backtick (`` ` ``), or `,`
are **silently dropped** (`valid_alias`).

### Git cache

Clones land under `$XDG_DATA_HOME/compat/repos` (fallback
`~/.local/share/compat/repos`), keyed by host/path segments. Materialization is
background (`reference_cache`). Override GitHub remotes with
`COMPAT_REPO_CLONE_GITHUB_BASE_URL` (see environment table above).

### Permission and prompt effects (security)

Every resolved reference **path** is layered onto the turn's permission snapshot
as:

```text
Rule { action: ExternalDirectory, resource: "<dir>/*", mode: Allow }
```

so tools may read/write/shell under that tree **without** an
`ExternalDirectory` permission prompt for those paths
([`run_turn_with_external_dirs`](../crates/hya-core/src/engine/turn.rs)).

References that carry a non-empty `description` are also injected into the
system prompt as sorted `<available_references>` / `<reference>` blocks (name,
path, description), which changes model behavior.

Example bag fragment:

```json
{
  "references": {
    "docs": "./docs",
    "sdk": { "path": "~/src/sdk", "description": "Shared SDK checkout" },
    "upstream": {
      "repository": "github.com/example/lib",
      "branch": "main",
      "description": "Upstream library"
    }
  }
}
```

