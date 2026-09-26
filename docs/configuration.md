# Configuration

hya reads its own YAML config from:

1. `$XDG_CONFIG_HOME/hya/config.yaml` (when that file exists)
2. `$HOME/.config/hya/config.yaml`

The file is parsed strictly as YAML via `serde_norway::from_str`
([`crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs)). JSON and
TOML are **not** accepted. Unknown top-level keys are ignored without warning —
there is no `deny_unknown_fields` — so a misspelled section such as `provider:`
instead of `providers:` is silently dropped. After editing, verify with
`hya models`. A file that is empty or whitespace-only is treated exactly
like a missing file and hya runs offline.

If no configured or discovered live model row resolves, hya publishes exactly
`hya/offline`, served by `DevProvider` as a local echo model. It is removed when
any live row exists. The same config file also drives tools, MCP servers,
plugins, permissions, subagent limits, model categories, and formatter status.

## First-Run / Offline Behavior

On startup, hya tries to load `config.yaml` (see
[`../crates/hya-app/src/config.rs`](../crates/hya-app/src/config.rs) `load()`
and `config_path()`). `cargo build` only compiles the workspace and does not
write user config. When a `hya` command starts and no file exists, hya
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

Normal startup never searches, opens, or imports Claude, Codex CLI, Grok CLI,
OpenCode, or another product's configuration. First run creates the starter
Hya config without an import offer.

How to tell you are offline:

- Every catalog surface shows exactly `hya/offline` with offline metadata.
- Assistant replies echo the input and say that no live provider is available
  and a provider must be configured.

## Agent Selection

`default_agent` (top-level `config.yaml` key, `Option<String>`) is the agent
stable id used for a new **root** Session when nothing more specific selects
one. It applies uniformly across every path that creates a root Session:

- `hya serve` — `POST /v1/sessions` without an explicit `agent` id, and the
  default row markers on `GET /v1/catalog/agents`.
- Headless commands — `hya exec`, `hya run`, `hya -p` goal mode, `hya loop`,
  `hya rpc`, and `hya workflow run` when it creates a new Session (see
  [CLI Reference](cli.md#hya-exec)).

Precedence: an explicit per-invocation agent id (a request field on `serve`;
no CLI flag yet on the headless commands) outranks `default_agent`, which
outranks the built-in `build` agent used when `default_agent` is unset. A
`default_agent` that does not name a selectable agent in the bound catalog is
a hard error (`UnknownAgentId`) at session-creation time — it does not
silently fall back to `build`.

```yaml
# Optional: agent profile selected when a workdir does not specify one.
# Falls back to the built-in `build` agent when omitted.
default_agent: build
```

## Remembered Agent Models

Agent model selection covers primary Agents, ordinary subagents, and hidden
`title`, `summary`, and `compaction` Agents. Defaults and temporary choices have
separate owners: user configuration files, remembered defaults in the backend
Session database, and temporary overrides in the root Session event stream.
Attached and remote clients update the backend's state, not the client's files.
The `title` Agent's model is the one that titles new root sessions after
their first prompt (see [Runtime — Session Titles](architecture/runtime.md#session-titles));
with no model chosen for it, the session's own model is used.

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
fallback). Bundle Agents use their bundle's `config.yml`; see
[Bundle configuration files](#bundle-configuration-files) for where it lives.
Agents in one WorkflowBundle or AgentSetBundle share that file. Both files use
the same model leaf:

```yaml
agents:
  hya-main:
    model: openai/gpt-4.1
```

Use the bundle Agent's stable id instead of `hya-main` in a bundle file. Saves
lock and reread the file, then atomically replace only the model leaf, preserving
unrelated settings (including the bundle's own keys), credentials, reasoning
fields and file permissions. Prepared bundle content is never rewritten.

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

## Bundle configuration files

Every bundle has exactly one user-owned configuration file, `config.yml`. The
file holds the bundle's Agent model defaults (`agents.<agent-id>.model`, see
above) and any keys the bundle's own process, MCP server, or sidecar reads.
Hya never writes bundle content into it, and it is not part of the bundle's
identity. It exists so that you can configure an installed bundle without
repackaging it.

### Where the file lives

| Bundle scope | Configuration directory | File |
| --- | --- | --- |
| User install (`hya bundle install`) and first-party/builtin bundles | `<hya config dir>/bundles/<percent-encoded-bundle-id>/` | `config.yml` in that directory |
| Project install (`hya bundle install --project`) | The bundle's source directory, `.hya/bundles/<dir>/` | `config.yml` in that directory |

`<hya config dir>` is the directory that holds the active `config.yaml`
(`$XDG_CONFIG_HOME/hya`, with the existing `$HOME/.config/hya` fallback). The
bundle id is encoded as one canonical percent-encoded path segment:
`hya/plan-impl-review` becomes `hya%2Fplan-impl-review`. A project bundle
shadows a user install with the same id, so its project `config.yml` is the only
file read for that id.

A project bundle's `config.yml` is not bundle content. It (and its
`.config.yml.lock` / temporary siblings) never enters the bundle's prepared
sources, digest, or project fingerprint. A reinstall or upgrade keeps the
existing file, and a `config.yml` inside an incoming package is ignored.
`hya bundle remove --project` deletes the source directory, including its
`config.yml`. A user-scope file stays after `hya bundle remove`.

**Breaking (0.41.0):** Hya no longer reads the old
`<hya config dir>/agents/<encoded-bundle-id>/config.yml` Agent model file. Move
it to `<hya config dir>/bundles/<encoded-bundle-id>/config.yml`. The `agents:`
leaf is unchanged.

### Example

To pin the model of the `plan-impl-review` bundle's reviewer Agent and store a
bundle-owned setting (`review.strict`), write
`~/.config/hya/bundles/hya%2Fplan-impl-review/config.yml`:

```yaml
agents:
  plan-impl-review-reviewer:
    model: openai/gpt-4.1
review:
  strict: true
```

A bundle provider that reads the file:

```python
import os, pathlib
config = pathlib.Path(os.environ["HYA_BUNDLE_CONFIG_FILE"])
settings = config.read_text() if config.exists() else ""
```

### Interface

| Contract | Value |
| --- | --- |
| `HYA_BUNDLE_CONFIG_DIR` | Absolute configuration directory. It might not exist yet. |
| `HYA_BUNDLE_CONFIG_FILE` | Absolute `config.yml` path inside that directory. It might not exist yet. |
| Receivers | `extensions.process` providers (Rust, Bun, and Claude kinds, and the implicit Bun process of a JavaScript Plugin), bundle-declared stdio MCP servers (`resources.mcp`), and Agent activation Bun sidecars. |
| Expansion tokens | `${BUNDLE_CONFIG_DIR}` and `${BUNDLE_CONFIG_FILE}` expand in process argv, MCP argv, and MCP `env` values, like `${BUNDLE_ROOT}`. |
| MCP precedence | Inherited `PATH` < `HYA_BUNDLE_CONFIG_*` < declared MCP `env` keys < `HYA_BUNDLE_ROOT` (host-owned). |
| Restart rule | At each root binding, Hya compares the content digest of every loaded process/MCP bundle's `config.yml` (or its absence) with the digest the bundle's providers started with. On a mismatch, that bundle's providers restart, as they do when the bundle changes. Bundles without a process or MCP server aren't restarted. Sidecars read the file on each activation. Agent model defaults load at startup. |
| Format | YAML mapping. Only `agents.<id>.model` is interpreted by Hya; other keys belong to the bundle. |

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
# name used by `hya login <id>` and shown as the provider in model refs.
providers:
  anthropic:
    kind: anthropic                      # openai-completion | openai-response | openai-codex | grok-build | anthropic | google
    base_url: https://api.anthropic.com/v1
    # Inline key is optional. Forms: literal, {env:VAR}, or {file:/path}.
    # A token saved via `hya login anthropic <token>` takes precedence.
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

# Plugins. These run process-wide. Project plugins are discovered per
# registered Project from <root>/.hya/plugins/<name>/plugin.toml of every root
# (one directory deep — not recursive); an entry here beats a manifest with
# the same id.
plugins:
  memory:
    command: [python3, memory.py]        # stdio JSON-RPC process
    timeout_ms: 500
    env:
      TOKEN: literal-token               # NOT templated — see Plugins
  ext:
    kind: bun                          # rust (default) | bun | other
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
    # `hya login grok <token>` — hya does not read `~/.grok/auth.json`.
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

A provider's **effective model list** is its remote model list (from the
[model cache](#model-cache-and-config-overrides)) merged per model id with
its `models:` entries:

- Every provider is discoverable and refreshable. A non-empty `models:` list
  no longer disables the remote list; it pins entries and overrides metadata.
- `models:` entries are trimmed and exactly deduplicated. They are routable
  without a model-list request and remain routable when no credential
  exists.
- A provider with neither cached remote rows nor `models:` entries makes one
  bounded request during startup. Providers with no cached rows, and
  discovery-only providers (absent, empty, or blank-only `models:`), also
  refresh in the background after startup (a `catalogUpdated` stream frame
  follows).
  Fetched rows are written to the model cache, never into `config.yaml`.
  Authentication headers are sent only when Hya has a credential.

### Model cache and config overrides

The model cache is a SQLite database at
`$XDG_CACHE_HOME/hya/model_cache.db` (fallback `~/.cache/hya/model_cache.db`).
It stores, per provider, each model the remote `/models` list returned: model
id, display name, context limit, output limit, reasoning default and
variants, tool support, and the fetch time. It replaces the old YAML
`models.yml.cache` beside `config.yaml`: the first open of an empty cache
imports that file once, and Hya no longer writes it (you may delete it).
Deleting `model_cache.db` is safe; it only costs one model-list request per
provider on the next start.

The cache is refreshed by startup discovery (see above), `hya models
--refresh`, and the v1 `RefreshProvider` / `UpsertProvider` routes (the TUI
Provider View's refresh and add-provider actions). A refresh replaces the
provider's rows; a 401/403 clears them; a transport, status, or decode failure
keeps the old rows.

**Merge rule (per model id, field by field).**

| Model is in | Source shown | Metadata |
| --- | --- | --- |
| the cache only | `remote` | the cached values |
| `models:` only | `config` | the entry's values; kind defaults for unset fields |
| both | `override` | each field set in the entry wins; each unset field falls back to the cached value |

For a model in both, the entry's `name`, `limit.context`, `limit.output`,
and `reasoning` override the cached display name, limits, and effort menu
individually. When a configured limit and a cached one contradict each other
(`output` above `context`), the cached value is dropped. The source appears in
the v1 `ModelSummary.source` field (`remote`, `config`, `override`, or
`offline`) and in `hya models --verbose`.

**Model entry fields.** A `models:` entry is a string id or a mapping:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string (required) | Provider-local model id; may contain `/` and `:`. |
| `name` | string | Display name shown by pickers and the Provider View. |
| `limit.context` | positive `u32` | Context window (see [Model limits](#model-limits)). |
| `limit.output` | positive `u32` | Max output tokens (see [Model limits](#model-limits)). |
| `reasoning` | `true`, `false`, or `{default?, variants?}` | `false` advertises no effort menu (same as `variants: []`); `true` keeps the remote or provider-kind menu; the mapping is described in [Reasoning metadata](#reasoning-metadata). |
| `modalities.input` | list of names (`text`, `image`, ...) | Whether the model accepts images: with `image` in the list, prompt image attachments are sent; without it, a prompt turn with attachments is refused with `invalid_argument` before anything runs. Unset means unknown, and attachments are allowed. Shown as v1 `ModelSummary.imageInput`. `modalities.output` is accepted and ignored. See [Prompt attachments](protocol/README.md#prompt-attachments-images). |

```yaml
providers:
  openrouter:
    kind: openai
    base_url: https://openrouter.ai/api/v1
    models:
      - id: vendor/model-a:free      # override: remote row + these fields
        name: Model A (free)
        limit:
          output: 8192               # context still comes from the remote list
      - id: my-finetune              # config-only row
        reasoning: false
        modalities:
          input: [text]              # no images: attachments are refused
```

The [TUI Provider View](tui.md#provider-view) (`/key`) writes these entries for you: adding a model or
editing a model's metadata calls `PUT /v1/providers/{id}/models`, which
patches that entry in `config.yaml`: fields the request omits keep their
value, an empty name or a `0` limit removes it (a string entry becomes a
mapping when fields are added and turns back into a string when none remain;
`reasoning` can be set but only cleared by removing the entry). Removing the
override calls `DELETE /v1/providers/{id}/models?modelId=…`. Adding a
provider calls `PUT /v1/providers/{id}`, which writes `kind` and `base_url`
(a new provider gets `models: []`) and saves the key to `auth/<id>.yaml`.
An edit that would make the config invalid is rejected and leaves the file
unchanged. Every change applies to the running server immediately. See
[How hya edits `config.yaml`](#how-hya-edits-configyaml) for what the writers
change in the file text. Routes and schemas: [Protocol guide — Providers and
keys](protocol/README.md#providers-and-keys).

### How hya edits `config.yaml`

Some commands write `config.yaml` for you: the Provider View routes (`PUT
/v1/providers/{id}`, `PUT /v1/providers/{id}/models`, `DELETE
/v1/providers/{id}/models`) and OAuth login (`hya login`). These writers
change only the lines of the entries they touch and keep everything else in
the file as you wrote it: comments (whole-line and `key: value  # note`),
blank lines, key order, quoting, indentation, CRLF line endings, and a
leading byte-order mark.

| Edit | What changes in the file |
| --- | --- |
| New provider | A `<id>:` block (`kind`, `base_url`, `models: []`) is appended after the last provider. `providers:` is added at the end of the file when missing; `providers: {}` becomes a block. |
| Existing provider | The `kind` and `base_url` values are replaced on their own lines (a quoted value stays quoted); `models: []` is added when the provider has no `models` key. Other keys are untouched. |
| Set a model field | The field's value is replaced in place, or the field is added after the entry's last field. A plain `- <id>` entry becomes `- id: <id>` with the new fields below it; an inline comment on it stays on the first line. |
| Clear a model field | The field's lines are removed (`limit:` goes when its last limit is cleared). An entry left with only `id` turns back into `- <id>`. |
| New model | `- <id>` is appended after the provider's last model entry; `models: []` becomes a block list. |
| Remove a model | Only the entry's own lines are removed (its `- ` line through its last field, including comments inside it). Comments above it stay. Removing the last entry writes `models: []`. |

New lines follow the file's indentation: the nesting step and whether list
items are indented under their key (`models:\n  - a`) or not
(`models:\n- a`) are taken from the existing file (default: two spaces,
indented lists).

A value the writer cannot edit in place — a flow-style provider such as `gw:
{kind: openai, …}`, a `models: [a, b]` flow list, or a block scalar — is
rewritten in block style for that one entry only; the rest of the file is kept.

The writer falls back to rewriting the **whole** file (which drops comments
and normalizes formatting) only when the file uses YAML it cannot map to
lines safely: anchors (`&name`), aliases (`*name`), tags (`!tag`), merge keys
(`<<:`), YAML directives or several documents, quoted or plain values that
span several lines, multi-line flow collections, tab indentation, or mixed
line endings. The server logs a warning (`config.yaml uses YAML the minimal
editor does not handle`) with the reason when this happens.

Every edit is checked before the file is replaced: the edited text must parse
to exactly the configuration the change intends (otherwise the edit fails and
the file is left untouched) and must pass config validation. The file is
replaced atomically (a temporary file in the same directory, then a rename)
and keeps its permissions.

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
| `max_attempts` | `HYA_PROVIDER_RETRY_MAX_ATTEMPTS` | `3` | Total request attempts per streamed completion, shared by pre-stream retries (transport, 429, 5xx) and the zero-event replay window (link-level failures and transient in-stream error frames). Clamped to at least 1. |
| `backoff_base_ms` | `HYA_PROVIDER_RETRY_BACKOFF_BASE_MS` | `100` | Exponential backoff seed; grows `2^attempt` with jitter (75–125%). |
| `backoff_max_ms` | `HYA_PROVIDER_RETRY_BACKOFF_MAX_MS` | `30000` | Ceiling for the exponential backoff and `Retry-After` waits (the latter is additionally hard-capped at 30 s). |

The budget covers both recovery layers: the pre-stream attempt loop, and the
zero-event replay window — when an established response dies before delivering
any event to the consumer (truncated body, connection reset, idle stall before
the first frame, or an in-stream error frame classed as rate limit, overload,
or 5xx such as `{"error":{"type":"rate_limit_error",…}}` after an HTTP 200),
the whole request is re-issued while budget remains. Non-transient error frames
(invalid request, auth, unclassified) are never retried. Once a single event has
been delivered the strict no-replay boundary applies and errors surface exactly
once. See [In-stream error frames](architecture/providers.md#in-stream-error-frames).

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

### Model limits

An object-form model entry may declare the model's token limits with an
optional `limit` block. hya otherwise knows nothing about a configured model's
real window: its catalog row (`ModelSummary`, `hya models --verbose`) reports
the context and output limits as unknown (omitted), the runtime sizes the
compaction threshold against a 200k-token fallback window, and Anthropic
routes send `max_tokens: 4096` whenever the agent does not ask for a specific
value — enough to truncate a long report with `finish: length`. Likewise a
model whose entry has no `reasoning` field and whose remote row publishes no
effort metadata reports reasoning as unknown, while still accepting the
provider kind's effort variants.

```yaml
providers:
  12th:
    kind: anthropic
    base_url: https://api.12th.day/v1
    api_key: "{env:TWELFTH_API_KEY}"
    models:
      - glm-5.3                     # plain string: no limits, 4096 fallback
      - id: glm-5.3-flash
        limit:
          context: 1048576          # 1M context window
          output: 131072            # gateway accepts max_tokens in [1, 131072]
```

| Field | Type | Meaning |
| --- | --- | --- |
| `limit.context` | positive `u32` | Context window in tokens. Reported on this model's catalog row (`contextLimit`) and drives the compaction threshold in place of the 200k runtime fallback. |
| `limit.output` | positive `u32` | Max output tokens. Becomes the default and the ceiling for the request's max-tokens field on this model. |

Both fields are optional; an omitted field stays unspecified. Validation fails
the config load when a value is zero, negative, fractional, non-numeric, or
larger than `4294967295`, when `output` exceeds `context` (both set), when
`limit` is not a mapping, or when it contains any key other than `context` and
`output`. Plain string entries remain valid and carry no limit. Remote
models whose cached row has a non-zero output limit (from the provider's model
list) follow the same rules, and an entry's `limit` fields override the cached
ones field by field.

**Request max tokens.** The rule is the same for every provider kind:

1. When the model's output limit is known (a configured `limit.output`, or a
   non-zero cached output limit for a remote model), a request without an
   explicit max-tokens value sends the limit, and an explicit value (compaction
   summaries, titles, plugin or model policy) larger than the limit is clamped
   to it. Anthropic sends it as `max_tokens`, OpenAI-compatible Chat as
   `max_tokens`, Responses (`openai-response`, `openai-codex`, `grok-build`) as
   `max_output_tokens`, and Google as `generationConfig.maxOutputTokens`.
2. When no output limit is known, the request is unchanged: Anthropic falls back
   to `max_tokens: 4096` (the field is required), and the other kinds omit the
   field unless the request carries an explicit value.

**Anthropic thinking.** A thinking budget from the reasoning effort (see the
table above) still requires `max_tokens` above the budget. A value at or below
the budget is raised to `budget + 4096`, but never past a known `limit.output`.
When the limit cannot hold the budget, the budget shrinks to leave
`min(4096, limit / 2)` answer tokens (for example `limit.output: 8192` with
`high` sends `max_tokens: 8192` and `budget_tokens: 4096`); when that leaves
less than Anthropic's 1024-token minimum, thinking is omitted.

Some upstreams reject a request whose input plus `max_tokens` exceeds the
context window. Set `limit.output` to what the endpoint accepts for a single
request, not a marketing figure, and keep it well below `limit.context` for such
providers.

### OAuth login (`openai-codex` and `grok-build`)

Interactive OAuth is implemented entirely in Rust:

```sh
# ChatGPT / Codex subscription (Codex default: device-code, print URL, no auto-open browser)
hya oauth login --provider codex --type openai-codex
# same commands on the TypeScript launcher:
hya oauth login --provider codex --type openai-codex
# optional: open the verification URL, or use localhost PKCE instead of device-code
#   --browser
#   --loopback --browser

# xAI SuperGrok / Grok CLI (device-code flow)
hya oauth login --provider grok --type grok-build --no-browser
hya oauth login --provider grok --type grok-build --no-browser

hya oauth status
hya oauth status
```

On success hya:

1. Writes an OAuth credential bundle to the Hya auth directory (see
   [Auth Tokens](#auth-tokens)).
2. Optionally fetches a catalog only to improve login confirmation output.
3. Upserts the non-secret Hya provider declaration (`kind`, `base_url`) and
   preserves any existing `models` and inline `api_key` fields. Only those
   lines change; comments and formatting in `config.yaml` are kept (see
   [How hya edits `config.yaml`](#how-hya-edits-configyaml)).

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
   hya oauth login --provider <name> --type <openai-codex|grok-build>
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

Credentials are **self-contained in hya config / auth** (`hya oauth
login` or `hya login`). hya never reads `~/.grok/auth.json`.

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

You can still paste a bearer with `hya login grok <token>` or an
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
hya login anthropic "$ANTHROPIC_API_KEY"
hya oauth login --provider codex --type openai-codex
hya auth list
hya oauth status
hya auth logout anthropic
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

Writes are atomic: a temp `.<provider>.yaml.tmp` is created with mode `0600` on
Unix (never readable by others), written, then renamed into place. If YAML deserialization yields no credential,
hya scrapes a bare `token: "..."` line and unquotes it as an API credential, so
a hand-written one-line file still works.

HTTP auth headers are marked sensitive and redirects are disabled so a secret is
not forwarded to another host.

Over the v1 API, `PUT /v1/auth/{providerId}` (`{"apiKey": "..."}`) writes a
static key file (`type: api`, mode `0600`) and `DELETE /v1/auth/{providerId}`
removes it; either change applies to the running server at once — the server
rebuilds that provider's route and catalog and emits a `catalogUpdated` stream frame, so no
restart is needed. Saving a key for a provider with no cached remote models
also fetches its model list. `GET /v1/auth` (`ListProviderAuth`) answers
`{"providerIds": ["anthropic", ...]}`: the sorted ids that have an
`auth/<id>.yaml` file. It never returns secret values; `GET /v1/providers`
shows each provider's `keySource` (`saved`, `oauth`, `config`, or `none`).
The OpenTUI frontend's Provider View uses these routes (see [TUI](tui.md) and
[Protocol guide — Providers and keys](protocol/README.md#providers-and-keys)).

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
hya --model gpt-5.5 exec "summarize the architecture"
hya models
hya models gateway --verbose
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
`--yolo` replaces the effective model with `danger` before engine construction,
which also makes `yolo` the default [session permission mode](#session-permission-modes).

### Session permission modes

A **permission mode** switches how one session tree (a root session and all
of its subagent sessions) answers permission checks at runtime, without
restarting the backend or editing `config.yaml`. The configured `permission`
block above stays the process policy; the mode only selects how each tool
call's check is derived from it.

| Mode | Behavior |
| --- | --- |
| `manual` | Asks go to the user (the TUI or another v1 client). Uses the configured model, except that a process-level `danger` (`--yolo` or `model: danger`) is treated as `default`, so `manual` really asks. |
| `yolo` | Allow immediately, bypassing every check including explicit Deny rules — the same as `danger`/`--yolo`, for this session tree only. |
| `<bundle-id>/<mode-id>` | `manual`, plus an approver: after the `permission.ask` hooks all defer, the declaring bundle's `permission.approve` hook answers the ask; `defer`, an error, or a timeout falls through to the user ask. Bundles declare these modes with `permission_modes:` (see [Agent bundle authoring](agent-bundle-authoring.md#permission-modes-permission_modes)). |

**Default.** A tree without a recorded mode uses `yolo` when the process runs
with `--yolo` or `permission.model: danger`, and `manual` otherwise.

**The user's own shell commands never ask.** A direct shell turn (`!command`
in the TUI, `CreateTurn` with `shell` in the v1 API) runs a command the user
typed, so it is approved once without a prompt in every mode — `manual`,
`yolo`, and bundle modes (the bundle's `permission.approve` approver and the
`permission.ask` hooks are not consulted for it). An explicit Deny rule still
blocks it (the call fails with a `permission` error, no prompt), a
`tool.execute.before` hook can still veto it, and a check for a directory
outside the working directory (`ExternalDirectory`) still asks. `bash` calls
the model issues are unaffected and ask as the mode says.

**Switching.** Set the mode on any session of the tree; it is recorded on the
root session as a `session_permission_mode_set` event, so it survives replay
and restarts and every subagent session inherits it:

```sh
# HTTP (the same call is Session.UpdateSession over gRPC)
curl -X PATCH http://127.0.0.1:8080/v1/sessions/$SESSION \
  -H 'content-type: application/json' -d '{"permissionMode":"yolo"}'
# List the selectable modes (built-ins first, then bundle modes)
curl http://127.0.0.1:8080/v1/permission-modes
```

The mode is read at every permission check, so a switch applies to the next
tool call of every session in the tree, including turns that are already
running. A tool call that was already authorized keeps running under the
decision it got. Switching to `yolo` also allows (once) every permission ask
of the tree that is still waiting for an answer, so blocked turns continue.
An unknown mode, or a bundle mode that no installed bundle declares, is
rejected with `invalid_argument`. If a recorded bundle mode's bundle is later
removed, the tree behaves as `manual` until the mode is changed.

Interfaces: `UpdateSessionRequest.permission_mode` (optional string),
`SessionInfo.permission_mode` (the effective mode; children report the root's
mode), `SessionUpdated.permission_mode` on the root session's event stream,
and `Catalog.ListPermissionModes` (`GET /v1/permission-modes`, rows
`{id, title, description, source}` with `source` = `builtin` or the bundle
id). See [`docs/protocol/api-reference.md`](protocol/api-reference.md).

**Saved "allow always" grants.** Answering a permission ask with *allow
always* in a client (TUI `2`, or `POST /v1/interactions/{id}/respond` with
`{"permission": {"allowed": true, "persist": true}}`) saves a grant in the session database (`saved_permission`). Saved
grants apply to every session of the server process, survive restarts (they
are reloaded when the server starts), and are listed and deleted with
`/rules` in the TUI or `GET` / `DELETE /v1/permissions/rules` — a deleted
grant stops applying at once. They are not written to `config.yaml`; see
[Protocol guide — Saved permission rules](protocol/README.md#saved-permission-rules).
The exception is access to a directory outside the session's Project roots
(ADR-0026): *allow always* there remembers only that directory (`<dir>/*`,
subdirectories included) and only for the session's Project; other Projects
still ask, and deleting the Project deletes the grant. A temporary session (or
one without a Project) keeps such a grant for itself until the server stops;
it is not saved. Older saved grants that allowed every outside directory keep
working for every session until you delete them.

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
| `HYA_MCP_BACKGROUND_AFTER_MS` | Foreground budget in milliseconds for `mcp__` tool calls. A call still running past the budget moves to the background: the turn gets a `[backgrounded]` tool result immediately, the real result is delivered later as a steered `[background job …]` user prompt, and `hya serve` runs the reclaim turn when the session is idle. Unset, `0`, or unparsable disables backgrounding (every call stays synchronous). | unset | `crates/hya-app/src/runtime.rs`, `crates/hya-core/src/engine/turn.rs` |
| `HYA_BUN_ADAPTER_DIR` | Path to an alternate Bun extension adapter checkout (`kind: bun` plugins). | Resolution order: this env override, executable-adjacent `../lib/hya/bun-adapter`, then workspace `crates/hya-plugin-bun/adapter`. | `crates/hya-app/src/plugins.rs` |
| `HYA_CLAUDE_ADAPTER_DIR` | Path to an alternate Claude Code adapter checkout (`kind: claude` plugins and `bundle install --claude`). | Resolution order: this env override, executable-adjacent `../lib/hya/claude-adapter`, then workspace `crates/hya-plugin-claude/adapter`. | `crates/hya-app/src/plugins.rs` |
| `HYA_BACKEND_BIN` | Binary under test for the `startup-bench` xtask; overrides the default `hya serve` target. | workspace `target/{profile}` binary | `crates/xtask/src/startup_bench.rs` |
| `HYA_STARTUP_TRACE` | When `1` or `true` (case-insensitive; any other value off), `hya serve` emits newline-delimited JSON startup phase marks to stderr, the last after the listen line: `{"hya_startup":true,"mark":"backend_listen","wall_ms":…,"detail":"<url>"}`. Mark names are listed in [troubleshooting](troubleshooting.md#diagnosing-slow-startup). | off | `crates/hya-app/src/startup_trace.rs`, `crates/hya-backend/src/serve.rs` |

### Bun adapter (`HYA_*`)

Read by the bundled Bun extension adapter
([`crates/hya-plugin-bun/adapter`](../crates/hya-plugin-bun/adapter)):

| Variable | Effect | Default / notes | Source |
| --- | --- | --- | --- |
| `HYA_BUNDLE_CONFIG_DIR` / `HYA_BUNDLE_CONFIG_FILE` | The bundle's configuration directory and `config.yml` (see [Bundle configuration files](#bundle-configuration-files)), visible to loaded extensions via `process.env`; the factory argument itself is always a frozen empty object. | set by the host for bundle processes and sidecars | `initialize.ts` |
| `HYA_DIRECTORY` | Extension working directory context. | `process.cwd()` | `initialize.ts` |
| `HYA_WORKTREE` | Worktree root context. | same as directory | same |
| `HYA_SERVER_URL` | `serverUrl` context for extensions that call back into the v1 API. | `http://127.0.0.1:0` | same |
| `HYA_PROJECT_ID` | Project id context. | worktree path | same |

### Related non-`HYA_` variables

| Variable | Effect | Source |
| --- | --- | --- |
| `BUN` | Bun binary used to run the bundled Bun adapter. | `crates/hya-app/src/plugins.rs` |
| `SHELL` | Shell program for PTY sessions on the v1 PTY routes; also listed among shell candidates. Defaults to `/bin/sh` when **unset**. A variable that is set but empty is **not** replaced — PTY create may receive an empty command. | `crates/hya-server/src/support/pty_shell.rs` |
| `HYA_REPO_CLONE_GITHUB_BASE_URL` | Overrides the GitHub base URL when cloning reference repositories (Enterprise / internal mirror). Trailing slashes trimmed. Default remote is `https://github.com/<path>.git`. Store under `$XDG_DATA_HOME/hya/repos` (else `~/.local/share/hya/repos`). | `crates/hya-server/src/support/reference_repository.rs` |
| `HYA_TERMINAL` | **Output only:** set to `1` in every PTY child environment so programs can detect the hya terminal. hya never reads it. | `crates/hya-server/src/support/pty_state.rs` |

## MCP Servers

hya supports **stdio/local** and **remote HTTP** MCP servers. `mcp.<name>.command`
is an argv array for a local stdio server; `mcp.<name>.url` connects to a remote
server over **Streamable HTTP** (the 2025-06-18 default) or, with
`transport: sse`, the classic HTTP+SSE transport.

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
3. `hya serve` watches for that marker: if the session is idle it
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

## Tool Naming Contract

Contributed tool names follow one composition rule so every provider-facing
name is unambiguous:

| Source | Provider-facing name |
| --- | --- |
| Built-in tools | bare names (`read`, `bash`, …) — reserved |
| Plugins (`plugins:` / `plugin.toml`) | `{plugin_id}__{local}` — composed by the host from the declared local name |
| MCP servers | `mcp__{server}__{local}` — composed from the server key and the server-declared tool name |
| Bundle tools | view-scoped (sidecar activation) — the bundle-local resource id and its `bundle:<bundle_id>/<kind>/<local>` stable id are the dispatch and permission spellings (see [AgentBundle authoring](agent-bundle-authoring.md)) |
| Bundle schema claims | the scheme's `canonicalTool` is the owning tool's `bundle:<bundle_id>/tool/<local>` stable id |

Rules enforced at configuration load and at runtime publication:

- The `mcp.<server-key>` key must contain only ASCII letters, digits, `-`, and
  `_` (no `__`); malformed keys are rejected at startup.
- A plugin/MCP tool whose name cannot compose into its qualified spelling
  (invalid tokens, or an `mcp`/`harness`/`builtin` head from a non-MCP source)
  is rejected in one grouped publication error listing every conflict.
- Aliases may shadow built-in or cross-source names (masking): a mask is
  resolved by scope — sources beat built-ins, and among sources the
  lexicographically greater source id wins. The built-in `read` is protected
  and can never be masked.
- External URI-scheme claims follow the same masking order across sources and
  are dispatched view-scoped: an agent's `read` serves `scheme://…` handles
  only when its compiled view also resolves the owning bundle tool.

## Bundle Schemas

Bundles may declare external URI-scheme extensions (`schemas:` in the bundle
manifest; see [Schema extensions](agent-bundle-authoring.md#schema-extensions-schemas)).
Two read-only surfaces report what is registered:

- **`hya bundle schema <BUNDLE_ID|PACKAGE>`** — one `SCHEME TOOL WRITABLE`
  row per schema that single bundle declares (builtin, user, or project scope,
  or a package file).
- **`GET /v1/runtime/schemas`** — the live published scheme table with its
  config `generation`, each row carrying `scheme`, `owner` (the winning source
  id, e.g. `bundle:hya/schema-demo`), `canonicalTool` (the owning tool's
  `bundle:<bundle_id>/tool/<local>` stable id), `writable`, and the full
  `chain` of claimants in ascending source order.

The table publishes with the installed-catalog generation: a freshly installed
bundle's schemes appear after the next bound turn, exactly like the rest of its
resources.

## Plugins

Plugins come from two places:

- **Config plugins** — the `plugins:` block of `config.yaml`. They start with
  the process (in the server's working directory) and serve every session.
- **Project plugins** — **`<root>/.hya/plugins/<name>/plugin.toml`** of a
  registered Project's roots (**one directory deep** — nested `plugin.toml`
  files are never found). They serve only that Project's sessions: their
  tools, Skills, and hooks (`permission.ask` included) never reach temporary
  sessions, directories outside every Project, the project-less global view,
  or another Project. Nothing is read from the directory the process started
  in.

Project plugins load lazily: no process starts until the Project's first bind
(its first session turn, or a catalog read for a directory inside it). Every
root is scanned in order and the **first root wins** by plugin id; a later
root's manifest with the same id is skipped with a warning. A config plugin
with the same id always wins over a manifest (the manifest is skipped with a
warning). Each project plugin is spawned with its **Project root** (the root
holding its `.hya/plugins` directory) as its working directory, so a relative
`command` such as `["python3", ".hya/plugins/memory/plugin.py"]` resolves
against that root; a crash respawn uses the same directory.

A change to a Project's plugin inputs — any `plugin.toml` added, removed, or
edited, or a plugin directory added or removed — **respawns that Project's
plugins at its next bind**: the next turn gets fresh processes with the new
declaration, and the old processes stop once no running turn uses them. Other
Projects and config plugins are not touched. Project plugins stop when their
Project's scope is dropped (see [Catalog Scope Cache](#catalog-scope-cache))
or the Project's roots change. Workspace adapters declared by a project plugin
are not supported and are ignored with a warning; `--pure` loads no project
plugins.

```yaml
plugins:
  memory:
    command: [python3, memory.py]
    timeout_ms: 500
    env:
      TOKEN: literal-token
  ext:
    kind: bun
  cc:
    kind: claude
    plugin_dir: /path/to/claude-plugin   # Claude Code plugin source dir
```

Config entries support:

| Field | Meaning |
| --- | --- |
| `kind` | `rust`, `bun`, `claude`, or `other`; default is `rust`. |
| `command` | Process command for stdio JSON-RPC. |
| `plugin_dir` | Claude Code plugin source directory; required by `kind: claude` entries without an explicit `command`, ignored by other kinds. Entries without it are skipped with a notice. |
| `enabled` | Defaults to `true`; disabled entries are skipped. |
| `timeout_ms` | Optional per-call timeout in **milliseconds**. When omitted: **30 s**. Fixed non-configurable timeouts: initialize **5 s**, shutdown **1 s**. |
| `env` | Environment variables passed **verbatim** to the child. The `{env:VAR}` / `{file:/path}` secret templating supported by `providers.<id>.api_key` and `mcp.<name>.env` is **not** applied here. Export the variable in the parent shell instead. |

### Directory manifests

Layout: `<root>/.hya/plugins/<name>/plugin.toml` for each root of a
registered Project (see above), scanned from each **immediate** subdirectory
of `.hya/plugins` only, in directory-name order
([`crates/hya-app/src/plugins.rs`](../crates/hya-app/src/plugins.rs)
`project_plugin_specs` / `scan_manifests`; per-Project loading in
[`crates/hya-app/src/project_plugins.rs`](../crates/hya-app/src/project_plugins.rs)).
Two manifests of one root with the same id: the first directory wins, the
other is skipped with a warning. Missing or **unreadable** `plugin.toml` is
skipped **silently** (`read_to_string` failure → `continue` with no log). Only
an **unparseable** file prints
`hya: skipping plugin manifest <path> (<error>)` on stderr. Neither case fails
the bind; a project plugin that fails to start is logged and left out of the
Project's catalog until its manifest changes or the scope is rebuilt.

Example:

```toml
id = "memory"
kind = "rust"           # rust | bun | other; default rust
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
| `kind` | `rust` (default), `bun`, or `other`. |
| `command` | Required argv array. |
| `enabled` | Default `true`. |
| `timeout_ms` | Optional per-call override (ms). |
| `[[hooks]]` | Repeated tables: `name` plus optional `posture` (`safe` \| `open`). |

An **unknown** hook name is warned about and dropped; the manifest still loads
and the plugin runs without that hook. Manifest posture entries act as posture
overrides and survive the config/manifest merge; YAML config entries carry no
posture overrides.

### Config-over-manifest merge

Config plugins and project manifests are resolved separately (config
plugins process-wide at startup, manifests per Project at its first bind), with
the same rules as [`hya_plugin::config::merge`](../crates/hya-plugin/src/config.rs):

1. Config entries are used **first** (skipping any with `enabled: false`).
2. A Project's manifests are used only if their id is **not** declared by a
   config entry (enabled or not), was not claimed by an earlier Project root,
   **and** the manifest itself is enabled.

Consequences: config always beats a same-id manifest; config `plugins` is a
`BTreeMap`, so config-declared plugins fold in **lexicographic plugin-id order**
(not YAML source order); setting `enabled: false` in config does **not** re-open
the id for a manifest to claim — the plugin is simply absent.

For `kind: bun` entries without `command`, hya resolves the adapter in this
order: (1) `HYA_BUN_ADAPTER_DIR` when set, (2) the installed adapter adjacent
to the executable at `../lib/hya/bun-adapter`, and (3) the workspace adapter
at `crates/hya-plugin-bun/adapter`. Set `BUN` to choose the Bun binary. If
Bun is not available, that plugin is skipped.

`kind: claude` entries run the Claude Code adapter
([`crates/hya-plugin-claude/adapter`](../crates/hya-plugin-claude/adapter))
the same way: the adapter is spawned as
`bun run <adapter>/src/main.ts --plugin-dir <plugin_dir> --plugin-id <id>`
with the adapter resolved from (1) `HYA_CLAUDE_ADAPTER_DIR` when set, (2) the
installed adapter adjacent to the executable at `../lib/hya/claude-adapter`,
and (3) the workspace `crates/hya-plugin-claude/adapter`. The adapter
discovers the plugin source (`plugin.json`, flat or `.claude-plugin/` layout),
declares translated skills and `hooks/hooks.json` hook registrations on the
initialize handshake, translates Claude Code hook decisions (e.g. PreToolUse
`decision: "block"`) into hya veto/allow replies, and maps `.mcp.json`
servers to MCP declarations.

To install a Claude Code plugin permanently as a hya bundle, use
`bundle install --claude <dir>`; see [CLI](cli.md#bundle-commands).

### Hook name vocabulary

A plugin may declare these hook names in its initialize handshake
([`crates/hya-plugin/src/messages.rs`](../crates/hya-plugin/src/messages.rs)).
Default posture when the plugin omits one: **Safe** for `permission.ask`,
`permission.approve`, and `tool.execute.before`; **Open** for all others.

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
| `permission.approve` | Safe (failures defer to the user ask; only called on the bundle that declared the active [permission mode](#session-permission-modes)) |
| `model.fallback` | Open (always fail-open) |

These three names also parse and may appear in `plugin.toml` / initialize, but
the host **never dispatches** them (dead hooks — see
[Plugin protocol](plugin-protocol.md)):

| Hook name | Default posture | Status |
| --- | --- | --- |
| `goal.evaluate` | Open | Dispatched before the bundle/model fallback evaluator |
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
leaves the **original** effort in place rather than clearing it. The params also
carry `root_session` (the root of the session's spawn tree) and `agent` (the
session's stable agent id), so a plugin can keep one decision per request
chain; see [Plugin protocol](plugin-protocol.md#chatparams).

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

## Custom Commands

Built-in slash commands are served by the backend command catalog over
`GET /v1/commands` (see [CLI Reference](cli.md#backend-command-catalog)). This
section covers **user-defined** prompt commands.

### Disk markdown commands

hya scans exactly two project-local roots
([`command_sources.rs`](../crates/hya-server/src/support/command_sources.rs)
`disk_commands`):

1. `<workdir>/.hya/command/**/*.md`
2. `<workdir>/.hya/commands/**/*.md`

Files are collected **recursively** and sorted by path. The slash-command name is
the path **relative to the discovery root**, with path segments joined by `/` and
the `.md` suffix stripped — e.g. `.hya/command/git/commit.md` becomes
`/git/commit`, not `/commit`. There is **no** user/home tier and no
inline-config command table.

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

Clones land under `$XDG_DATA_HOME/hya/repos` (fallback
`~/.local/share/hya/repos`), keyed by host/path segments. Materialization is
background (`reference_cache`). Override GitHub remotes with
`HYA_REPO_CLONE_GITHUB_BASE_URL` (see environment table above).

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
