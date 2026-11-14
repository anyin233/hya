# hya-extra bundles

`hya-extra/*` are optional bundles shipped alongside hya, under
`bundles/extra/`. They are not installed by default and are not part of the
twelve trusted [first-party bundles](bundle-runtime.md#first-party-bundles);
each one is an ordinary public `.hyabundle` package that you build and
install like any other bundle described in
[Bundle Authoring](agent-bundle-authoring.md). They exist for two
reasons at once: they are useful optional capabilities, and each one is a
coverage fixture exercised by `crates/hya-bundle/tests/extra_bundles.rs` and
the `crates/hya-e2e` process suite (`T2.26`–`T2.28` and `T2.30` in the
[Agent feature matrix](testing/agent-matrix.md)).

Every `hya-extra/*` bundle follows the same identity rule as the first-party
bundles: `identity.id` starts with `hya-extra/`, `identity.publisher` is
`hya-extra`, and source manifests use `version: 0.0.0` with
`version_ref: backend`, which preparation resolves to `[backend].version` in
`versions.toml`.

## `hya-extra/zvec-grep`

### Introduction

A `Plugin` bundle that wires [zvec-grep](https://github.com/zvec-ai/zvec-grep)
into hya as a bundled stdio MCP server, plus a Skill that teaches an agent
when semantic search is worth reaching for instead of `grep`/`rg`. zvec-grep
indexes a workspace (code and other text material) and answers natural-
language retrieval questions; this bundle exposes only its narrow
`agent`-toolset surface (`zvec_grep_search`) so installed agents cannot
create, rebuild, or drop an index.

### Usage

Prerequisites:

- Node.js >= 22
- `npm install -g @zvec/zvec-grep` (installs the `zg` CLI on `PATH`)
- Build an index once per workspace with `zg index` before semantic search
  returns results; the Skill tells the agent to ask you to do this rather
  than doing it itself

Package and install:

```sh
cargo run -p xtask -- package-bundle bundles/extra/zvec-grep zvec-grep.hyabundle
hya bundle install zvec-grep.hyabundle
```

Configuration file (only needed if you want to override the daemon's
defaults; this bundle reads none of its own keys today):
`<hya config dir>/bundles/hya-extra%2Fzvec-grep/config.yml` for a user
install, or `.hya/bundles/zvec-grep/config.yml` for a `--project` install.
See [Bundle configuration files](configuration.md#bundle-configuration-files).

### Interface

| Contract | Value |
| --- | --- |
| MCP resource | `resources.mcp` id `zvec-grep`, argv `zg server --stdio --mcp-toolset agent`, `timeout_ms: 600000` |
| Tool name (full-plane agent, e.g. `hya-main`) | `zvec-grep__mcp__zvec-grep__zvec_grep_search` |
| Tool name (a bundle agent that selects this server, e.g. `hya-extra/scout`) | `<local-server-id>__zvec_grep_search` (the bundle chooses the local id) |
| Tool input | `{"root": "<absolute path>", "query"?: string, "queries"?: [...], "fts"?: [...], "limit"?: number, ...}` — `root` is required and must be absolute |
| Skill id | `zvec-grep` (`resources/skills/zvec-grep/SKILL.md`) |

## `hya-extra/scout`

### Introduction

An `Bundle` defining one subagent, `scout`: a cheap
retrieval agent for orchestrators to spawn with "where/what/how is X"
questions about the local workspace. It answers with file:line evidence
gathered through its own `zvec-grep` MCP server (bundle agents cannot see a
sibling Plugin bundle's resources, so `scout` ships its own copy of the MCP
server declaration) plus the read-only `read`/`grep`/`glob` harness tools. It
has no write, edit, or shell access. Its manifest lists only these domain
tools: like every agent, `scout` receives its coordination tools (`report`,
`wait`, `send`, `list_channel`, channel reads) from the harness at startup, so
it can read and answer its lead's mail and then report. It has no spawn rights,
so it never sees `task` or `archive`.

### Usage

Prerequisites: same as `hya-extra/zvec-grep` above (`zg` on `PATH`, an index
built with `zg index`).

Package and install:

```sh
cargo run -p xtask -- package-bundle bundles/extra/scout scout.hyabundle
hya bundle install scout.hyabundle
```

Once installed, any built-in agent (e.g. `hya-main`) can spawn it with the
`task` tool using `subagent_type: "scout"` — no `can_spawn` edit is needed,
because installing an `Bundle` makes its agents immediately
spawnable from the ordinary built-in roster.

By default `scout` runs on the `quick` model category. Map that category in
your own `categories:` config, or pin `scout`'s model directly in its bundle
config file:
`<hya config dir>/bundles/hya-extra%2Fscout/config.yml`
(or `.hya/bundles/scout/config.yml` for a `--project` install):

```yaml
agents:
  scout:
    model: openai/gpt-5.4-mini
```

### Interface

| Contract | Value |
| --- | --- |
| Agent id | `scout` (also `bundle:hya-extra/scout/agent/scout`) |
| Role / lifecycle | `subagent`; spawned as a resident actor (reports, then is archived; follow-up mail wakes it) |
| Model policy | `{category: quick, reasoning: low}` |
| `resource_view.allow` | `harness:tool/read`, `harness:tool/grep`, `harness:tool/glob`, and its own bundle-local `zvec-grep` MCP server (domain tools only) |
| Coordination tools (harness-allocated) | `report`, `wait`, `send`, `list_channel`, `read channel://…`; no `task`/`archive` (no `can_spawn`) |
| MCP resource | `resources.mcp` id `zvec-grep`, argv `zg server --stdio --mcp-toolset full`, `timeout_ms: 600000` |
| Tool name as `scout` sees it | `zvec-grep__zvec_grep_search` (also `zvec-grep__zvec_grep_index_status`, etc. from the `full` toolset) |
| Prompt | `prompts/scout.md` — search first with `zvec_grep_search`, verify with `read`/`grep`, keep tool calls few, answer with file:line citations |

## `hya-extra/jev-model-router`

### Introduction

A `Plugin` bundle that routes each model request to a model that fits the
task's difficulty. It runs a small Bun process that implements the
[`chat.params`](plugin-protocol.md#chatparams) hook. The first time it sees
a request chain, it asks [Jev](https://docs.typesafe.ai) (TypeSafe's
"System One" classifier, `POST /v1/systemone`) one `choice` question: which
of your configured tiers (for example `easy` / `medium` / `hard`) covers
this request? Then it rewrites `request.model` to that tier's model.

Why: most turns don't need your most expensive model. Jev is a fast, cheap
judge, so it adds one short HTTP call per chain rather than an LLM round
trip. That lets short questions and trivial edits run on a cheaper, faster
model, and design or debugging work keeps the strong one.

Why sticky: providers cache the prompt prefix per model. If the model
changed on every round, every request would miss the cache and pay full
input price again. So the router makes one decision per request chain
(`root_session`: a lead and all of its subagents) and reuses it for every
later round and turn without calling Jev again. Optionally, a new user
message can move the chain to a harder tier (`route.escalate`); the router
never moves a chain to an easier tier.

hya still owns routing after the rewrite. The rewritten model streams
through the normal provider route, and its configured fallback chain and
any `model.fallback` hook still apply.

### Usage

Prerequisites:

- `bun` on `PATH` (>= 1.2.21; hya's release pins 1.4.2). hya runs
  `bun run router.ts` as the bundle's `extensions.process`.
- A TypeSafe API key.
- Every tier model must be a model your hya providers can serve.

Package and install:

```sh
cargo run -p xtask -- package-bundle bundles/extra/jev-model-router jev-model-router.hyabundle
hya bundle install jev-model-router.hyabundle
```

Configure it in the bundle config file
`<hya config dir>/bundles/hya-extra%2Fjev-model-router/config.yml` (user
install), or `.hya/bundles/jev-model-router/config.yml` for a `--project`
install. See [Bundle configuration files](configuration.md#bundle-configuration-files).
The process has a cleared environment (no `HOME`), so it reads the key from
this file, not from an environment variable. For example:

```yaml
jev:
  endpoint: https://api.typesafe.ai/v1/systemone   # default
  model: jev-latest                                 # default
  api_key_file: /Users/me/.config/typesafe/api-key  # or api_key: ts-...
  timeout_ms: 2000                                  # default
  min_confidence: 0.5                               # default
route:
  from: []              # empty = route every request
  default_tier: medium  # used when Jev fails or is unsure
  stickiness: chain     # one decision per lead + subagents
  escalate: false
tiers:                  # ordered easy -> hard
  - name: easy
    model: openai/gpt-5.4-mini
    criteria: Short questions, lookups, trivial one-file edits
  - name: medium
    model: anthropic/claude-sonnet-5
    criteria: Normal feature work or bug fixes touching a few files
  - name: hard
    model: anthropic/claude-opus-5-5
    criteria: Cross-cutting design, concurrency, migrations, subtle debugging
```

The same example, with comments, ships as
`bundles/extra/jev-model-router/config.example.yml`. hya compares the file's
content digest at each root binding and restarts the router when it changes.
The restart also clears cached decisions. The target of `api_key_file` is not
part of that digest, so restart hya after you rotate the key.

With an empty `route.from`, every request is routed, including subagents
that pin their own model (for example `hya-extra/scout` on a `quick`
model). With `chain` stickiness, those subagents join their lead's tier. To
leave them alone, list only the models the router may replace, for example
`from: [anthropic/claude-opus-5-5]`. Then requests on any other model pass
through untouched and do not call Jev.

The router's `bun test` suite (pure logic: config validation, tier choice,
stickiness, escalation, and Jev wire parsing) runs from the bundle
directory:

```sh
cd bundles/extra/jev-model-router && bun test
```

### Interface

| Contract | Value |
| --- | --- |
| Bundle | `kind: Bundle`, `extensions.process: {kind: bun, command: [bun, run, '${BUNDLE_ROOT}/router.ts']}`. Only `router.ts` is packaged. |
| Plugin id / hook | `jev-model-router`; one hook, `chat.params`. No tools or Skills. |
| Hook input used | `request.model` (the `from` filter), `root_session` / `session` (stickiness key), `agent`, `request.messages` (newest `role: user` message), `request.system`, and `request.tools` (count only). |
| Hook output | `{"outcome": "continue", "request": <the same request with only model changed>}` |
| Jev request | `POST <jev.endpoint>` with `Authorization: Bearer <key>` and body `{"model": <jev.model>, "state": {"agent", "latest_user_message" (first 4000 chars), "system_prompt_head" (first 1000 chars), "tool_count", "message_count"}, "questions": {"difficulty": {"type": "choice", "instructions": ..., "criteria": {<tier name>: <tier criteria>}}}}` |
| Jev answer used | `answers.difficulty.choice` (must be a tier name) and `answers.difficulty.confidence` (must be at least `min_confidence`) |

Config fields (`config.yml`; hya's own `agents:` leaf and other unknown
top-level keys are ignored; unknown keys inside `jev`, `route`, or a tier
are rejected):

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `jev.endpoint` | string (URL) | `https://api.typesafe.ai/v1/systemone` | Jev System One endpoint. |
| `jev.model` | string | `jev-latest` | Jev model. |
| `jev.api_key` | string | — | API key. Set exactly one of `api_key` and `api_key_file`. |
| `jev.api_key_file` | absolute path | — | File that holds the API key. Surrounding whitespace is trimmed. |
| `jev.timeout_ms` | integer 1–20000 | `2000` | Timeout per Jev call. hya's 30 s hook timeout is the outer bound. |
| `jev.min_confidence` | number 0–1 | `0.5` | If Jev's confidence is below this, the router uses `default_tier`. |
| `route.from` | list of model refs | `[]` | If not empty, only requests whose incoming model is listed are routed. |
| `route.default_tier` | tier name | last (hardest) tier | Tier used when Jev fails, times out, is unsure, or returns an unknown option. |
| `route.stickiness` | `chain` \| `session` \| `none` | `chain` | Decision key. `chain` uses `root_session` (a lead and its subagents share one model). `session` gives each session its own decision. `none` asks Jev on every request, which is not cache-friendly. |
| `route.escalate` | bool | `false` | If true, a new user message in the key's owner session (the chain root for `chain`) asks Jev again. The chain moves only to a harder tier. Tool-result rounds and subagent prompts never trigger this. |
| `tiers[]` | list, at least 1 | — | Ordered easy → hard. Each tier is `{name, model, criteria}`; all three are non-empty strings and `name` is unique. `criteria` is the rubric text Jev sees for that option. |

Failure behaviour: the router always answers `initialize`, so a
misconfigured router cannot block runtime publication. Failures are
reported on stderr, and stdout carries only protocol frames:

- Missing or invalid config: every request passes through unchanged.
- Jev HTTP error (401/422/429/529/5xx), timeout, unparsable answer, or low
  confidence: the router uses `default_tier`. It caches that decision like
  any other, so a chain does not retry Jev on every round.
- Any other exception inside the hook: the request passes through unchanged.
- The decision cache keeps at most 1024 keys (least recently used are
  evicted). Concurrent first requests for one key share one Jev call.

Known limits:

- hya computes the compaction threshold from the pre-rewrite model's context
  window, because compaction runs before `chat.params`. Keep the tier models'
  context windows at least as large as the incoming model's.
- Workflow stages that stream through a Workflow model route
  (`stream_with_workflow_route`) ignore a `chat.params` model rewrite.

## `hya-extra/model-fallback`

### Introduction

A `Plugin` bundle that runs a small Bun process implementing the
[`model.fallback`](plugin-protocol.md#modelfallback-choose-the-next-model-before-a-stream-exists)
hook: when a model fails before its event stream opens, it picks the next
model to try from a chain you configure per model.

Relation to `categories:` chains: hya's static `default_model`/`categories:`
cross-model chain (see [Configuration](configuration.md)) is tried first, on
every round, whether or not this bundle is installed. The engine only asks
`model.fallback` once that chain can't advance any further — including for
error classes the chain never advances on, such as `auth`. This bundle is for
policy the static chain can't express: a fallback chain scoped to a
*particular* model, a choice of which provider-error classes should trigger a
fallback at all, and a per-round retry cap. If you only need one global
ordered list of candidate models for every request, `categories:` alone is
simpler; reach for this bundle when different models need different fallback
targets.

### Usage

Prerequisites:

- `bun` on `PATH` (>= 1.2.21; hya's release pins 1.4.2). hya runs
  `bun run fallback.ts` as the bundle's `extensions.process`; no adapter is
  injected for an explicit process command, so `fallback.ts` speaks the hya
  plugin protocol (NDJSON JSON-RPC 2.0 over stdio) directly.

Package and install:

```sh
cargo run -p xtask -- package-bundle bundles/extra/model-fallback model-fallback.hyabundle
hya bundle install model-fallback.hyabundle
```

Configure it in the bundle config file
`<hya config dir>/bundles/hya-extra%2Fmodel-fallback/config.yml` (user
install), or `.hya/bundles/model-fallback/config.yml` for a `--project`
install. See
[Bundle configuration files](configuration.md#bundle-configuration-files). An
absent or invalid file behaves like an all-empty config (every consult gives
up), never a startup failure. For example:

```yaml
# Fallback chain for anthropic/claude-opus-5-5, walked in order and skipping
# any model already tried this round.
chains:
  anthropic/claude-opus-5-5: [anthropic/claude-sonnet-5, openai/gpt-5.5]

# Chain used for a model with no entry above (empty = no fallback for it).
default: []

# Provider-error classes that trigger a consult at all. `any` matches every
# class listed in the Interface table below.
on: [retryable, unknown_model]

# Bundle-side cap on retries per round; clamped to the engine's 8-attempt cap.
max_attempts: 3
```

The router's `bun test` suite (pure logic: config parsing/validation and the
`decide` chain walk) runs from the bundle directory:

```sh
cd bundles/extra/model-fallback && bun test
```

### Interface

| Contract | Value |
| --- | --- |
| Bundle | `kind: Bundle`, `extensions.process: {kind: bun, command: [bun, run, '${BUNDLE_ROOT}/fallback.ts']}`. Only `fallback.ts` is packaged (`fallback.test.ts` is undeclared and stays out). |
| Plugin id / hook | `model-fallback`; one hook, `model.fallback` (fail-open: an error, timeout, or malformed reply always reads as give-up). No tools or Skills. |
| Hook input used | `error.class`, `attempt`, `tried` (chain-walk key is `tried[0]`, the ORIGINAL failing model of the round — not the model that just failed, so `A -> B -> C` always walks `A`'s chain). `session`, `root_session`, `agent`, `message`, and `model` are accepted but not otherwise used. |
| Hook output | `{"outcome": "retry", "model": "<provider/model>"}` or `{"outcome": "give_up"}` |

Config fields (`config.yml`; hya's own `agents:` leaf is ignored, this bundle
has no agents to pin a model for):

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `chains` | map of model ref → list of model refs | `{}` | Fallback chain for a specific ORIGINAL failing model, in try order. |
| `default` | list of model refs | `[]` | Chain used for a model with no entry in `chains`. |
| `on` | list of `retryable` \| `unknown_model` \| `auth` \| `invalid_request` \| `other` \| `any` | `[retryable, unknown_model]` | Error classes that trigger a fallback consult; `any` in the list matches every class. |
| `max_attempts` | integer 1–8 | `3` | Give up once the round's attempt count exceeds this. Values above 8 are clamped (the engine never makes more than 8 provider attempts in one round regardless). |

Decision (`decide(params, config)` in `fallback.ts`, exported for `bun test`):
give up if `error.class` is not in `on`, if `attempt` exceeds `max_attempts`,
if every candidate in the applicable chain is already in `tried`, or if the
config is missing or invalid (logged to stderr; `initialize` still succeeds).
Otherwise retry with the first chain entry not already in `tried`.

Semantics carried over from the hook itself (see
[Plugin protocol](plugin-protocol.md#modelfallback-choose-the-next-model-before-a-stream-exists)):
the hook is asked only before an event stream exists for the round — a
mid-stream failure surfaces once and is never replayed on another model — and
a turn on a Workflow route (`model:`/`fallback:` on a Workflow stage) never
calls it; the Workflow owns its own declared candidate list instead.

## `hya-extra/token-summary`

### Introduction

A `Plugin` bundle that reports per-model token consumption for a session
tree. It runs a small Bun process that reads the request-scoped
[`session.usage`](plugin-protocol.md#request-scoped-host-capabilities) host
capability and exposes it two ways: a session-scoped
[API endpoint](agent-bundle-authoring.md#api-endpoints-apis) `GET /usage`
(id `usage`) that any v1 client can call without running a turn, and an
agent tool,
`token_summary`, that lets a running agent (or an orchestrator inspecting a
subagent's spend) ask for the same numbers mid-conversation. Both answer with
input, cache creation, cache read, and output tokens per model, with output
further split into thinking and visible tokens wherever the provider
reported that split — Anthropic never does, so that split reads `null` for
any model with at least one such round, rather than guessing.

Why a session tree: hya turns commonly spawn subagents (`task`). The default
scope, `tree`, folds the bound session plus every descendant subagent
session recursively, so an orchestrator's own report already accounts for
everything its team spent; `scope=session` narrows to one session's own log
when that is what you want instead.

### Usage

Prerequisites:

- `bun` on `PATH` (>= 1.2.21; hya's release pins 1.4.2). hya runs
  `bun run summary.ts` as the bundle's `extensions.process`.
- No configuration file is needed.

Package and install:

```sh
cargo run -p xtask -- package-bundle bundles/extra/token-summary token-summary.hyabundle
hya bundle install token-summary.hyabundle
```

Call the endpoint over HTTP once a session exists (the bundle id is one
percent-encoded path segment, `hya-extra%2Ftoken-summary`); the response body
is the bundle's own JSON, with no envelope:

```sh
curl "$HYA_URL/v1/sessions/$SESSION/bundles/hya-extra%2Ftoken-summary/usage?scope=tree"
```

```json
{
  "session": "hysec_...",
  "scope": "tree",
  "generated_by": "hya-extra/token-summary",
  "models": [
    {
      "model": "anthropic/claude-sonnet-5",
      "input": 500,
      "cache_creation": 20,
      "cache_read": 50,
      "output": 200,
      "thinking": null,
      "visible_output": null,
      "unsplit_output": 200,
      "rounds": 3,
      "prompt_total": 570
    }
  ],
  "total": { "...": "same fields as a model row, without model" },
  "sessions": [
    { "session": "hysec_...", "agent": "hya-main", "models": [ "..." ], "total": { "...": "..." } },
    { "session": "hysec_...", "parent": "hysec_...", "agent": "hya-task", "models": [ "..." ], "total": { "...": "..." } }
  ]
}
```

Over gRPC the same call is `hya.v1.BundleApi.InvokeSessionBundleApi` with
`{ session, bundle: "hya-extra/token-summary", method: "GET", path: "/usage",
query: { scope: "tree" } }`; the reply's `status` is `200` and `body` holds
the JSON above.

Once installed, any agent that can reach the bundle's namespace can call the
tool directly, for example `hya-main`:

```
hya-main calls token-summary__token_summary({"scope": "tree", "format": "table"})
```

which returns a compact Markdown table (columns: model, input, cache
creation, cache read, output, thinking, visible, rounds; unknown thinking
renders as `—`) plus one line per subagent session.

### Interface

| Contract | Value |
| --- | --- |
| Bundle | `kind: Bundle`, `extensions.process: {kind: bun, command: [bun, run, '${BUNDLE_ROOT}/summary.ts']}`. `summary.ts` and `schemas/usage.json` are packaged (`summary.test.ts` is undeclared and stays out). |
| API endpoint | `apis: [{ id: usage, method: GET, scope: session, path: /usage, description: "Per-model token usage of the session tree", response_schema: schemas/usage.json }]` — `GET /v1/sessions/{session}/bundles/hya-extra%2Ftoken-summary/usage`. `hya bundle info hya-extra/token-summary` prints `api=GET session /usage id=usage response_schema=schemas/usage.json description=…`. |
| Tool name (full-plane agent, e.g. `hya-main`) | `token-summary__token_summary` |
| Config | None. |

Endpoint `GET /v1/sessions/{session}/bundles/hya-extra%2Ftoken-summary/usage`:

| Query param | Values | Default | Meaning |
| --- | --- | --- | --- |
| `scope` | `session` \| `tree` | `tree` | `root` is rejected — a session endpoint may only read its own session or its descendants, never the whole spawn-tree root; use the tool for that. |

| Status | Body |
| --- | --- |
| `200` | The usage JSON above; its JSON Schema is `schemas/usage.json`, published in `GET /v1/bundle-apis` as `responseSchema`. |
| `400` | `{ "error": "<reason>" }` for an unrecognized query key or `scope` value. |
| `500` | `{ "error": "<reason>" }` when the `session.usage` read itself fails. |

Host-side failures use the standard codes instead: an unknown session is
`session_not_found` (404), another method on `/usage` is
`bundle_api_method_not_allowed` (405), and a crashed or timed-out process is
`bundle_api_failed` (502) — see
[Bundle runtime](bundle-runtime.md#bundle-api-endpoints).

Tool `token_summary` input `{ "scope"?: "session" | "tree" | "root", "format"?: "table" | "json" }`
(default `tree` / `table`). `format: "json"` returns exactly the endpoint's
200 body shape above; `format: "table"` (default) returns the rendered Markdown
string. A capability failure or bad input answers `{ "ok": false, "output": "<reason>" }`
instead of crashing the process; diagnostics otherwise go to stderr only.

Response field mapping, from the `session.usage` capability's `UsageTotals`
invariant (`input` excludes cache; `output` includes thinking — see
[Plugin protocol](plugin-protocol.md#request-scoped-host-capabilities)) to
this bundle's endpoint/tool JSON:

| `session.usage` field | Endpoint/tool field | Notes |
| --- | --- | --- |
| `input` | `input` | Unchanged. |
| `cache_write` | `cache_creation` | Renamed for readability. |
| `cache_read` | `cache_read` | Unchanged. |
| `output` | `output` | Unchanged (thinking included). |
| `split.thinking` | `thinking` | `null` when `split.unknown != 0` for that row. |
| `split.visible` | `visible_output` | `null` under the same condition as `thinking`. |
| `split.unknown` | `unsplit_output` | Always a number; `0` means the split above is exact. |
| `rounds` | `rounds` | Unchanged. |
| `input + cache_read + cache_write` | `prompt_total` | The whole prompt, cache included. |

`models` (top-level and per-session) is sorted by `prompt_total + output`
descending, then by model name. `sessions` mirrors the capability's
breadth-first, root-first row order and carries `parent`/`agent` only when
known, plus `truncated: true` when the capability's 512-session cap cut the
tree (see [Plugin protocol](plugin-protocol.md#request-scoped-host-capabilities)).

The bundle's `bun test` suite (transform to the usage JSON, Markdown
rendering, the id-dispatching NDJSON reader that lets a host request
interleave with an outstanding capability reply, query/input validation, and
the `api/request` handler's status codes) runs from the
bundle directory:

```sh
cd bundles/extra/token-summary && bun test
```

## `hya-extra/orchestrator`

An optional main agent whose only model-facing tool is `workflow`. JavaScript
programs arrange specialised workers and an optional TypeSafe Jev judge. The
orchestrator has no file, search, command or edit tools; every external effect
comes from a worker or judge call with a source line and JSONL trace.

### Package and use

Requires Bun 1.4.2 on `PATH` and hya with nested `tool.call`, `workspace.state`,
agent `advertise`/`handoff` and process `timeout_ms` support. The bundle
packages a main-agent prompt and three specialised worker prompts.

```sh
cargo run -p xtask -- package-bundle bundles/extra/orchestrator orchestrator.hyabundle
hya bundle install orchestrator.hyabundle
hya
```

Select `/agent orchestrator` in the TUI. For headless runs, set
`default_agent: orchestrator` in hya's `config.yaml` before `hya exec`.

| Worker | Tools | Purpose |
| --- | --- | --- |
| `orchestrator-scout` | `read`, `grep`, `glob` | Find facts; cannot run commands or change files. |
| `orchestrator-runner` | `bash`, `read` | Run requested commands, including generators/builds that write files; no edits by hand. |
| `orchestrator-editor` | `read`, `grep`, `glob`, `write`, `edit` | Make changes; cannot run commands or tests. |

Each worker also receives harness coordination tools, including `report`.
An editor makes a change and a runner checks it. The runner's shell restriction
is a prompt rule, not a filesystem sandbox. All three workers declare
`handoff: deterministic`: an accepted report uses hya's projection-derived
handoff instead of another model call. Workflows never revive their workers,
so a model-written handoff would add cost without being read.

### Workflow interface

The tool accepts exactly one of:

```ts
{script: string, args?: object, save_as?: string}
{run: string, args?: object}
{list: true}
{show: string}
```

Start with `{"list":true}` to find reusable workflows. For example, run and save:

```json
{"script":"// Find files for a request\nreturn await agents.run({agent:'orchestrator-scout',prompt:args.request,result:{paths:'Relevant file paths'}});","args":{"request":"Find the exporter and its tests"},"save_as":"find-files"}
```

Reuse it with `{"run":"find-files","args":{"request":"Find the parser"}}`.
Names match `^[a-z0-9][a-z0-9-]{0,63}$`. Unknown names produce a tool error
listing stored names; malformed inputs describe the four accepted forms.
Inspect source and metadata with `{"show":"find-files"}` before correcting
it and saving again under the same name.

The script is an async function body. Its globals are:

- `args`: the supplied object, default `{}`.
- `agents.run({agent, prompt, description?, result?, model?, effort?, timeout_secs?, cache?})`:
  `agent` is required and must name one of the three workers above; `prompt` is
  a non-empty string. `result?: Record<string,string>` describes requested report
  fields. Timeout defaults to 1800 seconds and accepts `0..1800`. Resolves to
  `{handle, session, status, report, result}`, where `status` is `done`, `failed`,
  `cancelled` or `timeout`, `report` is rendered channel text, and `result` is an
  object or `null`. `cache?: boolean` defaults to enabled; a remembered answer
  additionally has `cached: true`.
- `models.classify({state, questions})`: typed judge questions; returns `{answers}`.
  Question and answer schemas are defined below.
- `text(value)`, `console.log(...values)`, and `return value`: collect output.

Worker `result` fields append this instruction after a blank line:

```text
End your report with one final line of the form RESULT: {...} — a single-line JSON object with exactly these fields:
- <field>: <description>
```

The last `RESULT:` line is parsed, including one following a `[sender]` prefix.
Missing/invalid JSON, a non-object or a missing requested field yields `null`.
A result is parsed even without requested fields. `Promise.all` supports
independent concurrent calls.

`tools`, `ALL_TOOLS` and `describeTool` are removed. Touching `tools` explains
that a workflow can only call `agents.run` and `models.classify`. Imports,
`require`, `process`, `Bun`, `fetch` and timers are unavailable. The VM runs in
a worker thread so deadlines cover CPU loops after `await`. This is effect
recording discipline, not a security boundary. The runtime's `tool.call`
gate permits only `task`, `wait`, `archive` and `read` with a `channel://` path.
It also calls the separate read-only host operation `workspace.state`; it
never discovers host tools.

Only a first-line `// @options: {"timeout_ms":1800000,"max_output_chars":3000}`
comment sets options. Both must be positive integers; maxima are 3000000 ms
and 3600 characters respectively. The host process timeout is 3600000 ms.

### Remembered observations

A worker that finishes with `status: "done"` and leaves the workspace content
unchanged made an observation. The runtime remembers its complete report and
parsed result across scripts and sessions, saving another worker call when
the same question is asked on the same content.

Before starting a worker, the runtime calls `workspace.state({})`. Its state
is a SHA-256 identity of the containing git work tree's content: tracked and
untracked non-ignored files, excluding the top-level `.hya/` directory. A null
state (outside git or unavailable git state) disables reuse and storage. The
runtime checks state again after the worker returns; a changed or null state,
failure, cancellation, timeout or abandonment is never stored.

The lookup key is SHA-256 of the JSON array
`[before, agent, exactPrompt, model ?? null, effort ?? null]`, where
`exactPrompt` includes the appended RESULT instruction. Description and timeout
do not affect the key. Records live at
`<HYA_BUNDLE_CONFIG_DIR>/memo/<key>.json` (system temp when unset):

```ts
type Observation = {
  key: string; state: string; agent: string; description: string; prompt: string;
  status: "done"; report: string; result: Record<string, unknown> | null;
  handle: string; session: string; trace: string; seq: number; stored_ms: number;
};
```

A matching record with the same state resolves to the original
`{handle, session, status, report, result}` plus `cached: true`, starting no
worker. Its trace line ends `-> cached from <handle>, 0.0 s`, and one line
after `</trace>` says `reused observations: <n>` only when at least one answer
was reused. The trace's `source` names the original handle, session, absolute
trace file and sequence number; every agent record carries its state fields.

Use `cache: false` for observations that depend on time, network or other state
outside the project files. It skips both lookup and storage while still
recording workspace states:

```js
return await agents.run({agent: "orchestrator-runner",
  prompt: "Check the remote service status", cache: false});
```

### Store, results and traces

Every `save_as` writes the exact source, including a failing script, to
`<HYA_BUNDLE_CONFIG_DIR>/workflows/<name>.js` and metadata to `<name>.json`:
`{name, description, saved_ms, runs, last_run_ms, last_status, last_error}`.
Names, descriptions and errors are strings; times are Unix epoch milliseconds,
`runs` is an integer, and status is `completed` or `failed`. Description comes
from the first `//` line, excluding an options line. A new/replaced source
starts with one run; subsequent stored runs increment it and update the last
time/status/error. `last_error` holds the failure text, or an empty string
after completion. Saving a replacement overwrites the old source even on
failure, so the latest attempt can be inspected and repaired.

`list` returns `Stored workflows:` followed by a JSON array of
`{name, description, saved_ms, runs, last_status, last_error?}`; failed entries
include the error cut to 200 characters, preserving its start/end. `show`
returns JSON `{script: string, metadata: <the full metadata above>}` without
executing or updating metadata. Unknown `show` and `run` names give the same
error listing available names. The per-installation store survives sessions.
hya supplies the writable configuration directory for the bundle process.

Plugin replies are `{ok: boolean, output: string, time_ms: number}`. Text starts
with `Workflow completed in … s` or `Workflow failed in … s`; a stored run uses
`Workflow <name> completed in … s` (or `failed`). All saves add
`saved as <name>`. Output items and optional `<console_output>` precede the
`<trace>` of `agents.run` and `models.classify` calls. Failures retain partial
output and add `Script error: <message>`. Display output stays below 4800
characters, retaining output start/end and the first/last trace lines.

Full JSONL traces live at
`<HYA_BUNDLE_CONFIG_DIR>/traces/<session>/<call>.jsonl` (system temp when unset;
session/call components percent-encoded). The first record is
`{script, args, stored, options}`; `stored` names the stored source or save,
or is `null`. Call records have `seq`, `line`, `kind` (`agent` or `classify`),
`name`, `input`, `ok`, `output?`, `error?`, `started_ms`, `ms`; agent records
also carry `result`, `state_before` and `state_after` (state strings or `null`
when unknown), and `handle`/`session` once started. A hit has `cached: true`
and `source: {handle, session, trace, seq}` pointing to the original worker
and trace record; `state_after` is `null` because no worker ran. Input/output
are JSON strings cut to 4000 characters. The final record holds `{status, output}`
with the complete untruncated output. Calls are recorded in start order.
The call-record contract is:

```ts
type TraceCall = {
  seq: number; line: number; kind: "agent" | "classify"; name: string;
  input: string; ok: boolean; output?: string; error?: string;
  started_ms: number; ms: number;
  handle?: string; session?: string;
  result?: Record<string, unknown> | null; // Always present for agent records.
  state_before?: string | null; state_after?: string | null; // Always present for agents.
  cached?: true;
  source?: {handle: string; session: string; trace: string; seq: number};
  abandoned?: boolean; archived?: boolean; archive_error?: string;
};
```

Effects are not undone. Pending worker calls are abandoned and known workers
are archived before returning, with `abandoned`, `archived` and `archive_error`
recorded. Pending starts get up to five seconds to return an identity;
unresolved starts show `abandoned (start unknown)`. Workers whose own helper
timeout already returned remain live and can be managed through hya's
ordinary coordination tools.

### Classification and host interfaces

```ts
type Question =
  | {type: "bool"; instructions: string; criteria?: {true: string; false: string}}
  | {type: "choice"; instructions: string; criteria: Record<string, string>}
  | {type: "score"; instructions: string; criteria: string[]};
type Answer =
  | {type: "bool"; probability: number}
  | {type: "choice"; choice: string; probabilities: Record<string, number>; confidence: number}
  | {type: "score"; score: number; confidence: number};
```

`models.classify({state, questions})` accepts any JSON `state` and a non-empty
`Record<string, Question>`. It returns `{answers: Record<string, Answer>}`.
For example:

```js
return await models.classify({
  state: {report: "All exporter tests passed"},
  questions: {
    ready: {type: "bool", instructions: "Does the report support completion?"},
    next: {type: "choice", instructions: "Choose the next step", criteria: {repair: "Tests failed", finish: "Tests passed"}},
    quality: {type: "score", instructions: "Rate the evidence", criteria: ["Missing", "Partial", "Complete"]},
  },
});
```

The judge request is `POST <judge.endpoint>` with bearer authorization and
JSON `{model, state, questions}`. Boolean questions map to Jev `type:"noul"`
and answers map `answers[id].noul` to `{type:"bool", probability}`. Choice and
score answers retain their typed fields. Probabilities/confidence are in
`0..1`; scores are criterion indices in `0..criteria.length-1`. Missing judge
configuration, HTTP failures or malformed responses reject the call.

The process is `bun run ${BUNDLE_ROOT}/runtime.ts`, speaking plugin protocol
v1 over NDJSON stdio. Its `initialize` response declares only `workflow`, with
a JSON Schema `oneOf` for the four input forms (no additional fields). A
`tool/call` request carries `{tool:"workflow", input, session:string,
call:string, host_capability:string}`. Invalid input returns JSON-RPC
`-32602`; an unknown stored name or store error returns `-32003`. Script
failures return the ordinary tool reply with `ok:false` and partial output.

Worker coordination uses request-scoped `host/capability` with
`{capability, session, call, method:"tool.call", params:{name,input}}`,
receiving `{output:unknown}`. Workspace observations use the same envelope with
`method:"workspace.state", params:{}`, receiving `{state:string|null}`. The
string is 64 lowercase hexadecimal characters; null means the state is
unknown. This is a separate read-only host operation, never a `tool.call`
name. There is no tool discovery request.
`agents.run` sends `task({tasks:[{subagent_type:agent, prompt, description,
model?, effort?}]})`, waits using
`wait({targets:[handle],mode:"all",timeout_secs})`, and reads rendered report
text with `read({path:"channel://<DM id>"})` (or `channel://<handle>` if no DM
id is returned). Mail/stall wakes keep waiting; `finished` and
`already_finished` entries are terminal. Spawn failures use the returned
member summary. A helper timeout or user-stop cancellation without a report
returns an empty report and `result:null`. The main agent's resource view
contains only `workflow` plus harness coordination (`archive`, `list_channel`,
`report`, `send`, `task`, `wait`, mail-only `read`); only `workflow` is advertised.
Worker tool views are the explicit domain tools above plus harness coordination.

### Configuration and checks

The optional judge reads `HYA_BUNDLE_CONFIG_FILE`, normally
`<hya config dir>/bundles/hya-extra%2Forchestrator/config.yml`, or
`.hya/bundles/orchestrator/config.yml` for a project install. See the bundle’s
`config.example.yml`. Workers and stored scripts work without a judge;
`models.classify` rejects clearly until configured.

| Key | Type | Default |
| --- | --- | --- |
| `judge.endpoint` | non-empty string | `https://api.typesafe.ai/v1/systemone` |
| `judge.model` | non-empty string | `jev-1.14.0` |
| `judge.api_key` | non-empty string | Set exactly one key source. |
| `judge.api_key_file` | absolute path string | Read and trim the key from this file. |
| `judge.timeout_ms` | integer `1..3000000` | `20000` |

hya's optional `agents.<agent-id>.model` overrides are independent of the judge.
Unknown keys inside `judge` are rejected. Configuration is read at process
start; hya restarts it at the next root binding after a config edit. Restart
hya after changing only the key file.

Run `bun test` in `bundles/extra/orchestrator/` for offline fake-host/fake-Jev
and real-child stdio tests. Only `runtime.ts` and the four prompts are packaged;
tests, the bundle README and `config.example.yml` are development files.
The source README and configuration example live in `bundles/extra/orchestrator/`.
