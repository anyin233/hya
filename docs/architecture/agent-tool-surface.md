# Agent Tool Surface

This document describes the tools that hya exposes to agents, with detailed
coverage of file access, local search, interaction, network, and mailbox tools.
It distinguishes three related but different surfaces:

1. **Registered**: a name resolves in `ToolRegistry`.
2. **Advertised**: a canonical schema is included in a model request or Compat
   tool-list response.
3. **Executable**: the tool has the runtime plane, session, permissions, and
   other resources needed to complete a call.

The distinction matters because aliases resolve but are not advertised,
model filters hide some registered schemas, and several always-
registered builtins delegate to runtime planes that can be disconnected or
empty. The registry stores canonical tools and aliases separately, and only
canonical tools contribute schemas.
([crates/hya-tool/src/tool.rs:376-410](../../crates/hya-tool/src/tool.rs#L376-L410),
[crates/hya-tool/src/tool.rs:484-490](../../crates/hya-tool/src/tool.rs#L484-L490),
[crates/hya-tool/src/tool.rs:462-469](../../crates/hya-tool/src/tool.rs#L462-L469))

## Builtin inventory

`ToolRegistry::builtins()` installs 26 canonical schema names before model
filtering. The inventory below is complete for that constructor.
([crates/hya-tool/src/tool.rs:313-347](../../crates/hya-tool/src/tool.rs#L313-L347))

| Area | Canonical schema names | Role |
| --- | --- | --- |
| File access | `read`, `write`, `edit`, `apply_patch` | Read, replace/write, or patch workspace files. `apply_patch` and `edit`/`write` are advertised mutually exclusively for selected models. |
| Local discovery | `ls`, `glob`, `find`, `grep`, `lsp` | List directories, match paths, search text, or query language servers. |
| Commands | `shell`, `bash` | Two advertised names backed by the same shell implementation. |
| Human/session interaction | `question`, `ask_user`, `todowrite`, `plan_exit`, `invalid` | Ask structured or simple questions, update session todos, request a plan-mode transition, or represent invalid tool arguments. |
| Agents and teams | `skill`, `list_agents`, `task`, `send`, `roster`, `channels`, `join`, `leave` | Load skills, discover/spawn agents, and use team mail/channels. |
| Network | `webfetch`, `websearch` | Fetch a URL or run provider-backed web search. |

### Question and ask_user

`question` accepts a batch of structured questions under a top-level `questions`
array. Each item requires `question`, `header`, and `options` (each option is
`{label, description}`). Optional fields are `multiple` (allow several
selections) and `custom` (allow a free-text answer outside the option list;
defaults to true when omitted). The tool routes through the InteractionPlane and
returns chosen option labels; a question the user did not answer renders as
`Unanswered` rather than failing the call.
([crates/hya-tool/src/question.rs:11-16](../../crates/hya-tool/src/question.rs#L11-L16))

`ask_user` is a single free-text/select interaction. Parameters:

| Field | Role |
| --- | --- |
| `question` | Required prompt text. |
| `kind` | `"text"` (default path) or `"select"`. |
| `options` | Required non-empty list when `kind` is `"select"`. |
| `allow_custom` | With select, allow an answer outside the options. |
| `default` | Optional default for free-text. |

A cancelled or failed ask does **not** produce a tool error — it returns
`{"answer": "", "cancelled": true}`. Callers must inspect `cancelled` rather
than relying on an error. Contrast this with `question`, which renders
unanswered entries as `Unanswered`.
([crates/hya-tool/src/tool.rs:1059-1072](../../crates/hya-tool/src/tool.rs#L1059-L1072))

### Task

`task` launches one subagent or a multi-member team extension. Nested `task`
calls are allowed; recursion depth and total fan-out are bounded by
`SubagentGovernor` (see [`docs/configuration.md`](../configuration.md#subagent-limits)).
Execution requires a session and checks `Action::Task` for every member; the
spawner also enforces the caller's `can_spawn` roster (unknown or disallowed
agents surface as `unknown_agent_id` / `agent_spawn_not_allowed`). An empty or
omitted `subagent_type` normalizes to `"general"`; a non-empty unknown id does
not fall back to `general`.

| Parameter | Role |
| --- | --- |
| `description` | Short label (required with single-member form). |
| `prompt` | Work for the agent (required). |
| `subagent_type` | Agent id; empty/omitted normalizes to `"general"`. |
| `category` | Logical model-category override. |
| `model` | Concrete provider/model override (wins over category). |
| `task_id` | Resume an existing subagent session. Sentinels `new`, `null`, `none`, and `undefined` (case-insensitive), plus empty/whitespace, all mean "start fresh". |
| `command` | Optional command that triggered the task. |
| `background` | Bool; non-blocking single-member spawn. Multi-member background is rejected. |
| `resident` | Long-lived actor (non-blocking spawn) rather than a one-shot turn. |
| `inline_agent` | Request-scoped agent overlay (`name`, `prompt`, `description`, `category`, `model`, `resident`). Unsupported fields fail with `unsupported_inline_agent_field`. |
| `members[]` | hya extension: fan one call out to several subagents (each needs `prompt`; optional per-member overrides). |

([crates/hya-tool/src/task.rs:10-28](../../crates/hya-tool/src/task.rs#L10-L28))

### Skill

The only parameter is `name`. It must be a skill name from the
`available_skills` list injected into the system prompt (not a path). The tool
asserts `Action::Skill` on `Resource::Skill(name)`, then returns a
`<skill_content>` envelope containing the SKILL.md body, the skill's absolute
base directory as a `file://` URL (relative paths inside the skill resolve
against it), and a sampled `<skill_files>` listing capped at
`FILE_SAMPLE_LIMIT = 10` entries (recursive, sorted, excluding `SKILL.md`
itself). The output states that the file list is sampled, so a skill with more
than 10 supporting files shows an incomplete list. See also
[`docs/skills.md`](../skills.md).
([crates/hya-tool/src/skill.rs:14](../../crates/hya-tool/src/skill.rs#L14),
[crates/hya-tool/src/skill.rs:95-150](../../crates/hya-tool/src/skill.rs#L95-L150))

### Todowrite

Input is a `todos` array of `{content, status, priority}` objects. The call
**replaces** the session's whole todo list rather than appending. The list lives
in the in-memory `TodoPlane` (not persisted independently of the event log). The
result echoes the list back with a title carrying the count of still-open items
(status other than `"completed"`). Alias: `todo`.
([crates/hya-tool/src/todo.rs:75-136](../../crates/hya-tool/src/todo.rs#L75-L136))

### Mailbox tools

All mailbox tools report that they are available only inside a running team when
the mailbox plane is disconnected
([crates/hya-tool/src/mailbox.rs:243-248](../../crates/hya-tool/src/mailbox.rs#L243-L248)).

**`send`**: required `to` (teammate handle such as `reviewer-3`, or a channel with
a leading `#` such as `#build`) and `body`; optional `kind` =
`message` (default) | `announcement`. An empty/whitespace body is an input error.
Channel mail reaches every current subscriber. Result metadata returns the
resolved sender handle (`from`), the normalized recipient address (`to`), and the
`recipients` count.
([crates/hya-tool/src/mailbox.rs:250-312](../../crates/hya-tool/src/mailbox.rs#L250-L312))

**`roster`**: no parameters; returns one row per live teammate with `handle`,
agent type, session id, scheduling mode, `status` (`idle` | `busy` | `done` |
`failed`, folded in from `AgentActivityChanged` by the resident supervisor), and
`current_task`. Registered `ToolPermission::ReadOnly`, so it allows without
prompting under `default`.
([crates/hya-tool/src/mailbox.rs:314-390](../../crates/hya-tool/src/mailbox.rs#L314-L390))

**`channels`**: no parameters; lists every mail channel of the acting agent's team
with its member list and message count. Registered `ToolPermission::ReadOnly`.
([crates/hya-tool/src/mailbox.rs:393-437](../../crates/hya-tool/src/mailbox.rs#L393-L437))

**`join`**: takes a channel name; the leading `#` is optional (`#build` and
`build` are the same channel). It subscribes the acting agent and **creates** the
channel if it does not exist — there is no separate create-channel tool.
Registered `ToolPermission::Tool`, so it asks under `default`.
([crates/hya-tool/src/mailbox.rs:439-473](../../crates/hya-tool/src/mailbox.rs#L439-L473))

**`leave`**: takes a channel name (leading `#` optional) and unsubscribes the
acting agent. After leaving, channel posts no longer reach the agent but direct
handle mail still does. Registered `ToolPermission::Tool`.
([crates/hya-tool/src/mailbox.rs:475-504](../../crates/hya-tool/src/mailbox.rs#L475-L504))

`list_agents` enumerates definitions usable by `task`.
([crates/hya-tool/src/agents.rs:22-84](../../crates/hya-tool/src/agents.rs#L22-L84))

### Shell

`shell` and `bash` have distinct advertised names but wrap the same `ShellTool`.
The schema accepts a command, timeout, working directory, and environment; the
implementation uses `sh -c`, defaults to 120 seconds (`DEFAULT_TIMEOUT_MS =
120_000`), and caps returned command output at 16 KiB while saving the full
output under `.hya/tool-output/`. An optional `workdir` outside the session
workdir raises `Action::ExternalDirectory` on `<cwd>/*`.
([crates/hya-tool/src/tool.rs:313-347](../../crates/hya-tool/src/tool.rs#L313-L347),
[crates/hya-tool/src/shell.rs:17-65](../../crates/hya-tool/src/shell.rs#L17-L65),
[crates/hya-tool/src/shell.rs:38-181](../../crates/hya-tool/src/shell.rs#L38-L181),
[crates/hya-tool/src/shell.rs:250-261](../../crates/hya-tool/src/shell.rs#L250-L261))

### Webfetch

Parameters: `url` (http/https only), `format` = `text` | `markdown` (default) |
`html`, and `timeout` in seconds — default 30 s, clamped to a maximum of 120 s.
Responses larger than 5 MB are rejected. Responses whose content type is
jpeg/png/gif/webp are returned as base64 data-URI attachments instead of text.
The tool asserts `Action::WebFetch` on `Resource::Url(url)` and carries
`ToolPermission::Tool`, so under `permission.model: default` it asks before every
fetch.
([crates/hya-tool/src/webfetch/mod.rs:18-27](../../crates/hya-tool/src/webfetch/mod.rs#L18-L27))

### Websearch

Call parameters (in addition to the provider discussion below):

| Parameter | Default / values |
| --- | --- |
| `query` | Required. |
| `numResults` | Default **8** (Exa path). |
| `livecrawl` | `fallback` (default) \| `preferred`. |
| `type` | `auto` (default) \| `fast` \| `deep`. |
| `contextMaxCharacters` | Schema text advertises 10000; the Exa client forwards the field only when the caller supplies it. |

The tool is itself an MCP client: Exa is called at `https://mcp.exa.ai/mcp` with
the key appended as an `?exaApiKey` query parameter; Parallel at
`https://search.parallel.ai/mcp` with a bearer token. It asserts
`Action::WebSearch` on `Resource::WebSearch(query)`.
([crates/hya-tool/src/websearch.rs:16-18](../../crates/hya-tool/src/websearch.rs#L16-L18),
[crates/hya-tool/src/websearch.rs:132-171](../../crates/hya-tool/src/websearch.rs#L132-L171))

### Hidden aliases

Five legacy aliases resolve during execution but do not appear in
`ToolRegistry::schemas()`:

| Canonical advertised name | Hidden lookup alias |
| --- | --- |
| `webfetch` | `fetch` |
| `websearch` | `search` |
| `todowrite` | `todo` |
| `apply_patch` | `patch` |
| `plan_exit` | `plan` |

This behavior is explicit in registration and covered by a test that requires
the canonical names to be visible and the aliases to remain hidden.
([crates/hya-tool/src/tool.rs:313-347](../../crates/hya-tool/src/tool.rs#L313-L347),
[crates/hya-tool/src/tool.rs:484-490](../../crates/hya-tool/src/tool.rs#L484-L490),
[crates/hya-tool/tests/tool.rs:111-146](../../crates/hya-tool/tests/tool.rs#L111-L146))

There is therefore no advertised local-file tool named `search`. The hidden
name `search` resolves to **web search**, not GREP. Local discovery is exposed
as `glob`, `find`, and `grep`; provider-backed internet search is advertised as
`websearch` when enabled.
([crates/hya-tool/src/tool.rs:313-347](../../crates/hya-tool/src/tool.rs#L313-L347))

## Advertisement and naming

Before each completion request, hya obtains canonical registry schemas and
applies an advertisement-only filter:

- `use_patch` is true when the model string contains `gpt-`, does not contain
  `oss`, and does not contain `gpt-4`.
- `apply_patch` is advertised only when `use_patch` is true.
- `edit` and `write` are advertised only when `use_patch` is false.
- enabled `websearch` is advertised to every model provider.
- Every other canonical schema passes through.

The tools remain registered even when their schemas are filtered from the
request. In particular, `apply_patch` is the **only** file-mutation tool
advertised to gpt-* models under that filter (edit/write are hidden there).
([crates/hya-core/src/engine/turn/messages.rs:57-75](../../crates/hya-core/src/engine/turn/messages.rs#L57-L75))

### Why WEBSEARCH was provider-filtered

The removed `compat` restriction was inherited product policy, not a
model-protocol or tool-execution requirement.

The upstream OpenCode history is explicit. Commit
[`9c237f0`](https://github.com/anomalyco/opencode/commit/9c237f0bfb9335c8ce6c793c4eee0e17ef4d775e)
"temporarily restrict[ed] codesearch and websearch to opencode zen users" while
an enterprise opt-out was unresolved. Commit
[`419983c`](https://github.com/anomalyco/opencode/commit/419983c0f1dcffc4fae28f844e7658326e2ee5aa)
then restored an opt-in for non-Zen users through `OPENCODE_ENABLE_EXA`; its
[pull request](https://github.com/anomalyco/opencode/pull/5132) describes this as
an interim rollout rule. Current OpenCode keeps the same shape: web search is
enabled for its `opencode` provider or when explicit Exa/Parallel flags are set.
([current registry](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/tool/registry.ts),
[Parallel rollout](https://github.com/anomalyco/opencode/pull/26227))

hya commit `fd96760794056ce9eacaad9c6d72768863d890c6` copied the strict
`provider == "opencode"` branch. Commit
`07af114e9284ad3a79c62fa777cecb96a766e91f` later changed that provider string
to `compat` as part of a broad external-compat debranding change. It did not add
the upstream opt-in flags or establish `compat` as a web-search capability.

That distinction matters because hya's provider IDs are user-defined config
keys, and Compat config import preserves those IDs. A provider named `compat`
is therefore neither required nor sufficient to identify OpenCode Zen.
([crates/hya-app/src/config.rs](../../crates/hya-app/src/config.rs),
[crates/hya-app/src/config.rs](../../crates/hya-app/src/config.rs))

The execution path itself never examines the model provider. `tools.websearch`
selects Exa or Parallel, optionally overrides the endpoint and key, and can
disable the built-in. Exa is the enabled, unauthenticated default. Exa keys are
sent as `exaApiKey` query parameters; Parallel keys are sent as bearer tokens.
([crates/hya-tool/src/websearch.rs:22-72](../../crates/hya-tool/src/websearch.rs#L22-L72),
[crates/hya-tool/src/websearch.rs:157-171](../../crates/hya-tool/src/websearch.rs#L157-L171))

The stale compatibility condition was removed. Enabled websearch is now
advertised independently of the model provider.

The OpenAI, Anthropic, and Google request encoders preserve the canonical
`ToolSchema.name`; they translate only the surrounding provider JSON shape.
Descriptions and input schemas are forwarded with the same values.
([crates/hya-provider/src/openai.rs:23-75](../../crates/hya-provider/src/openai.rs#L23-L75),
[crates/hya-provider/src/anthropic.rs:15-57](../../crates/hya-provider/src/anthropic.rs#L15-L57),
[crates/hya-provider/src/google.rs:139-194](../../crates/hya-provider/src/google.rs#L139-L194))

The Compat tool-list implementation sorts canonical schemas by name and
preserves each schema name as the returned `id`. Its separate ID listing is
also sorted; both surfaces now reflect the configured registry without a
websearch model/provider filter.
([crates/hya-server/src/compat/experimental_tool.rs:9-36](../../crates/hya-server/src/compat/experimental_tool.rs#L9-L36),
[crates/hya-server/src/compat/experimental_tool.rs:39-57](../../crates/hya-server/src/compat/experimental_tool.rs#L39-L57))

## READ

### Input schema and path resolution

The advertised schema requires `filePath` and optionally accepts `offset` and
`limit`; it also lists `path` for compatibility. Runtime deserialization stores
the path in one optional field and accepts `filePath` as an alias, so direct
execution accepts either spelling even though provider-side schema validation
can require `filePath`. Both numeric fields advertise a minimum of zero.
([crates/hya-tool/src/read.rs:14](../../crates/hya-tool/src/read.rs#L14),
[crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98))

The workdir is absolutized and lexically normalized. Relative paths are joined
to that workdir; absolute paths remain absolute. Normalization removes `.` and
lexically pops one component for `..`; it does not canonicalize symlinks.
([crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98),
[crates/hya-tool/src/lsp_path.rs:4-11](../../crates/hya-tool/src/lsp_path.rs#L4-L11))

The external-directory boundary is therefore lexical. As an implementation
consequence, this check does not classify a symlink located inside the workdir
as external based on the symlink target.
([crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98),
[crates/hya-tool/src/lsp_path.rs:4-11](../../crates/hya-tool/src/lsp_path.rs#L4-L11))

READ performs two permission checks. A path outside the normalized workdir
requires `Action::ExternalDirectory` on the containing directory wildcard (or
the directory itself plus `*`), then every read requires `Action::Read` on the
resolved path. External-directory permission is checked before a missing-path
error is returned.
([crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98),
[crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98))

### File-kind dispatch

READ samples the first 4,096 bytes. PNG, JPEG, GIF, WebP, and PDF content is
returned as a base64 data-URL attachment. Detection prefers magic bytes and
falls back to the extension. Other binary files are rejected; the binary
heuristic checks a fixed extension list, NUL bytes, and a greater-than-30%
ratio of selected control bytes in the sample.
([crates/hya-tool/src/read_media.rs:9](../../crates/hya-tool/src/read_media.rs#L9),
[crates/hya-tool/src/read_media.rs:29-54](../../crates/hya-tool/src/read_media.rs#L29-L54),
[crates/hya-tool/src/read_media.rs:64-85](../../crates/hya-tool/src/read_media.rs#L64-L85),
[crates/hya-tool/src/read_media.rs:115-165](../../crates/hya-tool/src/read_media.rs#L115-L165))

The attachment result contains `title`, a short `output`, non-truncated
metadata, and one `attachments` entry with `type`, MIME type, and data URL.
Unsupported binary files fail with `Cannot read binary file: <path>` before
UTF-8 decoding.
([crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98),
[crates/hya-tool/tests/read.rs:215-267](../../crates/hya-tool/tests/read.rs#L215-L267))

### Text limits and output

Text reads are one-based. Omitted or zero `offset` becomes 1, and the default
line limit is 2,000. `limit` is not clamped, so zero collects no lines. An
offset beyond the counted lines is an error. UTF-8 BOM is stripped; remaining
invalid UTF-8 is returned as an I/O invalid-data error.
([crates/hya-tool/src/read.rs:14](../../crates/hya-tool/src/read.rs#L14),
[crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98),
[crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98),
[crates/hya-tool/src/read_text.rs:14-20](../../crates/hya-tool/src/read_text.rs#L14-L20),
[crates/hya-tool/src/read_text.rs:14-20](../../crates/hya-tool/src/read_text.rs#L14-L20))

Three independent bounds shape text output:

| Bound | Behavior |
| --- | --- |
| Requested/default lines | Stops collecting after `limit`; the default is 2,000. |
| Individual line length | Keeps 2,000 Unicode scalar values and appends `... (line truncated to 2000 chars)`. |
| Aggregate content | Stops before exceeding 50 KiB and tells the caller which `offset` to use next. |

The line and byte implementations are in
[crates/hya-tool/src/read_text.rs:9](../../crates/hya-tool/src/read_text.rs#L9)
and
[crates/hya-tool/src/read_text.rs:14-20](../../crates/hya-tool/src/read_text.rs#L14-L20);
tests pin both limits in
[crates/hya-tool/tests/read_limits.rs:56-117](../../crates/hya-tool/tests/read_limits.rs#L56-L117).

The result contains `title`, XML-like `output` with numbered lines, unnumbered
`content`, a 20-line `metadata.preview`, and display metadata for line start,
line end, counted total, and truncation. `metadata.loaded` is currently always
an empty array.
([crates/hya-tool/src/read_text.rs:14-20](../../crates/hya-tool/src/read_text.rs#L14-L20),
[crates/hya-tool/tests/read.rs:60-101](../../crates/hya-tool/tests/read.rs#L60-L101))

Two limit labels need careful interpretation:

- Truncating one overlong line does **not** set `metadata.truncated`; the suffix
  is the only indication.
- Once the 50 KiB loop breaks, `totalLines` is the number scanned through the
  first rejected line, not the actual total number of lines in the file.

Both follow directly from the collector and are pinned by tests that expect an
untruncated flag for a 2,001-character line and `totalLines == 52` for a
60-line file stopped at the byte cap.
([crates/hya-tool/src/read_text.rs:14-20](../../crates/hya-tool/src/read_text.rs#L14-L20),
[crates/hya-tool/tests/read_limits.rs:56-84](../../crates/hya-tool/tests/read_limits.rs#L56-L84),
[crates/hya-tool/tests/read_limits.rs:86-117](../../crates/hya-tool/tests/read_limits.rs#L86-L117))

### Directory reads and missing paths

For a directory, READ lists immediate children only, puts directories first,
sorts each group lexically, applies the same one-based offset and 2,000-entry
default limit, and returns directory-specific display metadata. The 50 KiB text
cap is not applied to this directory result.
([crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98),
[crates/hya-tool/tests/read.rs:103-140](../../crates/hya-tool/tests/read.rs#L103-L140))

A missing path produces up to three lexically sorted suggestions from its
parent when either lowercase filename contains the other; otherwise it returns
only `File not found`. The behavior is covered by a focused suggestion test.
([crates/hya-tool/src/read.rs:26-98](../../crates/hya-tool/src/read.rs#L26-L98),
[crates/hya-tool/tests/read_missing.rs:56-77](../../crates/hya-tool/tests/read_missing.rs#L56-L77))

## WRITE

### Schema and validation

The advertised WRITE schema requires `filePath` and `content`; it also lists
`path` for compatibility. Runtime deserialization stores the path in one optional
field and accepts `filePath` as an alias (`path` is runtime-only). Provider-side
schema validation can require `filePath`.
([crates/hya-tool/src/write.rs:21-96](../../crates/hya-tool/src/write.rs#L21-L96))

Paths use the same lexical workdir resolution as READ. An outside path requires
`Action::ExternalDirectory` on its parent wildcard (`<parent>/*`) before
`Action::Edit` on the resolved file
([crates/hya-tool/src/write.rs:21-96](../../crates/hya-tool/src/write.rs#L21-L96),
[crates/hya-tool/src/write.rs:21-96](../../crates/hya-tool/src/write.rs#L21-L96)).

### Create, BOM, and post-write processing

Parent directories are created as needed. An existing file's UTF-8 BOM is
preserved; a BOM present in the incoming content is also propagated (desired BOM
is source-or-incoming). After the write, the configured formatter runs; if the
formatter rewrites the file, the BOM is re-synced. The LSP plane is touched and
diagnostics are appended to the human-readable output and returned under
`metadata.diagnostics`.
([crates/hya-tool/src/write.rs:21-96](../../crates/hya-tool/src/write.rs#L21-L96))

## EDIT

### Schema and validation

The advertised EDIT schema requires `filePath`, `oldString`, and `newString`,
with optional `replaceAll`. Runtime deserialization also accepts the short
spellings `path`, `old`, `new`, and `replace_all`.
([crates/hya-tool/src/edit.rs:28-124](../../crates/hya-tool/src/edit.rs#L28-L124),
[crates/hya-tool/src/edit.rs:28-124](../../crates/hya-tool/src/edit.rs#L28-L124))

EDIT rejects identical old/new strings. An empty `oldString` is rejected for
an existing path, but for a missing path it creates parent directories and the
file from `newString`. This creation path is part of EDIT's implementation even
though WRITE is the explicit full-file/create tool.
([crates/hya-tool/src/edit.rs:28-124](../../crates/hya-tool/src/edit.rs#L28-L124))

Paths use the same lexical workdir resolution as READ. An outside path requires
`Action::ExternalDirectory` on its parent wildcard, and every call requires
`Action::Edit` on the resolved file before any content is changed.
([crates/hya-tool/src/edit.rs:28-124](../../crates/hya-tool/src/edit.rs#L28-L124),
[crates/hya-tool/src/edit.rs:28-124](../../crates/hya-tool/src/edit.rs#L28-L124))

### Matching and replacement

For an existing file, EDIT preserves its UTF-8 BOM and line-ending convention.
LF parameters are converted to CRLF before matching a CRLF file, and a BOM in
either the source or incoming replacement is retained exactly once.
([crates/hya-tool/src/edit.rs:28-124](../../crates/hya-tool/src/edit.rs#L28-L124),
[crates/hya-tool/src/edit_replace.rs:10-72](../../crates/hya-tool/src/edit_replace.rs#L10-L72),
[crates/hya-tool/tests/edit.rs:113-141](../../crates/hya-tool/tests/edit.rs#L113-L141),
[crates/hya-tool/tests/edit.rs:172-200](../../crates/hya-tool/tests/edit.rs#L172-L200))

Matching is deliberately broader than exact substring replacement. Candidates
are tried in this order: exact, line-trimmed, anchored block similarity,
whitespace-normalized, indentation-flexible, escape-normalized, trimmed
boundary, context-aware, and multi-occurrence. The first candidate family that
produces an acceptable match wins.
([crates/hya-tool/src/edit_replace/replacers.rs:1-12](../../crates/hya-tool/src/edit_replace/replacers.rs#L1-L12))

Without `replaceAll`, the selected candidate must occur exactly once. With
`replaceAll`, all occurrences of that selected candidate are replaced. EDIT
rejects missing matches, ambiguous matches, and fuzzy candidates whose matched
span is disproportionately larger than `oldString`.
([crates/hya-tool/src/edit_replace.rs:10-72](../../crates/hya-tool/src/edit_replace.rs#L10-L72),
[crates/hya-tool/src/edit_replace.rs:97-105](../../crates/hya-tool/src/edit_replace.rs#L97-L105))

Focused tests demonstrate line-trimmed, whitespace-normalized, anchored,
escape-normalized, trimmed-boundary, and context-aware matches. These are
contract behavior, not fallback behavior supplied by a provider.
([crates/hya-tool/tests/edit_fuzzy.rs:56-118](../../crates/hya-tool/tests/edit_fuzzy.rs#L56-L118),
[crates/hya-tool/tests/edit_fuzzy.rs:120-206](../../crates/hya-tool/tests/edit_fuzzy.rs#L120-L206),
[crates/hya-tool/tests/edit_fuzzy.rs:208-275](../../crates/hya-tool/tests/edit_fuzzy.rs#L208-L275))

### Post-edit processing and result

After writing, EDIT runs the configured formatter, restores the desired BOM if
the formatter changed it, touches the LSP plane, and collects diagnostics. A
formatter can therefore change the final file beyond the literal replacement;
a test explicitly installs a formatter that rewrites the whole file.
([crates/hya-tool/src/edit.rs:28-124](../../crates/hya-tool/src/edit.rs#L28-L124),
[crates/hya-tool/tests/edit.rs:143-170](../../crates/hya-tool/tests/edit.rs#L143-L170))

Success returns `created`, replacement count, relative `title`, human-readable
`output`, diagnostics, a unified diff, and addition/deletion metadata. The
shape and diff contents are covered by the EDIT result test. This result path
does not apply a local size cap to the diff or output.
([crates/hya-tool/src/edit.rs:28-124](../../crates/hya-tool/src/edit.rs#L28-L124),
[crates/hya-tool/tests/edit.rs:202-238](../../crates/hya-tool/tests/edit.rs#L202-L238))

## APPLY_PATCH

The parameter is `patchText` (runtime alias `patch`) carrying a Codex/Compat-style
patch envelope. Supported hunk kinds are **add**, **update**, **delete**, and
**move**.
([crates/hya-tool/src/apply_patch/mod.rs:16-23](../../crates/hya-tool/src/apply_patch/mod.rs#L16-L23),
[crates/hya-tool/src/apply_patch/apply.rs:59-121](../../crates/hya-tool/src/apply_patch/apply.rs#L59-L121))

Every path in the envelope must be relative and must not escape the session
workdir: an absolute path or a `..` component is an **input error**. Every
touched path (and move destination) is permission-checked as `Action::Edit`
**before** any file is written, so a denial leaves the whole patch unapplied.
([crates/hya-tool/src/apply_patch/mod.rs:16-23](../../crates/hya-tool/src/apply_patch/mod.rs#L16-L23),
[crates/hya-tool/src/apply_patch/mod.rs:16-23](../../crates/hya-tool/src/apply_patch/mod.rs#L16-L23))

The result is a Compat-style title plus an aggregate diff and per-file metadata.
After application, the same post-edit formatter + BOM re-sync + LSP-diagnostics
step as write/edit runs for non-delete paths.
([crates/hya-tool/src/apply_patch/mod.rs:16-23](../../crates/hya-tool/src/apply_patch/mod.rs#L16-L23))

As noted under advertisement, `apply_patch` is the only file-mutation tool
advertised to gpt-* models under the `use_patch` filter.

## LSP

Operations (exact `operation` enum values):

`goToDefinition`, `findReferences`, `hover`, `documentSymbol`, `workspaceSymbol`,
`goToImplementation`, `prepareCallHierarchy`, `incomingCalls`, `outgoingCalls`.

The call takes a file path (`filePath`) plus 1-based `line` and `character`
(converted to LSP 0-based internally), plus an optional `query` used only by
`workspaceSymbol`. The tool is `ToolPermission::ReadOnly`. It performs an
`Action::ExternalDirectory` check for files outside the workdir, then
`Action::Lsp` on the resolved path. When no language server is registered for the
file type, the tool returns a tool error whose message is
`No LSP server available for this file type.`
([crates/hya-tool/src/lsp.rs:15-26](../../crates/hya-tool/src/lsp.rs#L15-L26),
[crates/hya-tool/src/lsp.rs:29-125](../../crates/hya-tool/src/lsp.rs#L29-L125),
[crates/hya-tool/src/lsp_plane.rs:16-40](../../crates/hya-tool/src/lsp_plane.rs#L16-L40),
[crates/hya-tool/src/tool.rs:626-634](../../crates/hya-tool/src/tool.rs#L626-L634))

## Local search: GLOB, FIND, and GREP

### Shared implementation

GLOB and GREP are in-process Rust implementations; they do not invoke
`ripgrep`. Their recursive walker uses `std::fs::read_dir`, descends into
directories, returns files only, and silently skips directories it cannot
read. Both cap returned rows at `SEARCH_LIMIT == 100`.
([crates/hya-tool/src/tool.rs:180](../../crates/hya-tool/src/tool.rs#L180),
[crates/hya-tool/src/tool.rs:951-1002](../../crates/hya-tool/src/tool.rs#L951-L1002))

Path/include matching uses the same custom matcher as permission patterns. Its
only metacharacter is `*`; `?`, character classes, and path-aware `**`
semantics are not implemented. A pattern is tested against both the relative
path and basename.
([crates/hya-tool/src/permission.rs:432-460](../../crates/hya-tool/src/permission.rs#L432-L460),
[crates/hya-tool/src/tool.rs:716-805](../../crates/hya-tool/src/tool.rs#L716-L805))

Both tools resolve a relative `path` against the workdir, require an
action-specific permission on the pattern, and separately require
`ExternalDirectory` permission when the search root is outside the workdir.
([crates/hya-tool/src/tool.rs:716-805](../../crates/hya-tool/src/tool.rs#L716-L805),
[crates/hya-tool/src/tool.rs:814-944](../../crates/hya-tool/src/tool.rs#L814-L944),
[crates/hya-tool/src/tool.rs:814-944](../../crates/hya-tool/src/tool.rs#L814-L944))

### GLOB

GLOB requires `pattern` and optionally accepts a directory `path`, defaulting
to the workdir. Passing an existing file as `path` is an input error. It walks
recursively, matches files, sorts full paths lexically, and returns at most 100.
([crates/hya-tool/src/tool.rs:1010-1056](../../crates/hya-tool/src/tool.rs#L1010-L1056))

The result contains a relative `title`, count/truncation metadata,
human-readable absolute-path `output`, workdir-relative legacy `paths`, and the
pre-truncation `total`. No matches returns `No files found`.
([crates/hya-tool/src/tool.rs:1010-1056](../../crates/hya-tool/src/tool.rs#L1010-L1056),
[crates/hya-tool/tests/glob_grep.rs:83-129](../../crates/hya-tool/tests/glob_grep.rs#L83-L129))

`metadata.truncated` is computed as `total >= 100`, so it is true when there are
exactly 100 matches as well as when additional matches exist. It means the cap
was reached, not necessarily that a 101st result was found.
([crates/hya-tool/src/tool.rs:1010-1056](../../crates/hya-tool/src/tool.rs#L1010-L1056))

### FIND

FIND is a separate compatibility-oriented path matcher. It requires `pattern`,
optionally accepts `path`, uses the same recursive walker and `*` matcher, and
returns sorted `{path, size}` records. Unlike GLOB, this implementation has no
100-result cap and does not perform the shared external-directory check. A
supplied relative `path` is converted directly to `PathBuf`; it is not resolved
against `ToolCtx.workdir` as GLOB/GREP paths are.
([crates/hya-tool/src/tool.rs:814-944](../../crates/hya-tool/src/tool.rs#L814-L944))

### GREP

GREP requires a non-empty Rust regular expression `pattern`, optionally accepts
`path`, and optionally filters files with the custom `*`-only `include`
matcher. Invalid regex syntax is an input error.
([crates/hya-tool/src/tool.rs:814-944](../../crates/hya-tool/src/tool.rs#L814-L944))

If `path` identifies a file, GREP intentionally searches that file's **parent
directory**, not only the named file. The Compat behavior is explicit in code
and covered by a test where passing `src/main.rs` also returns a match from
`src/lib.rs`.
([crates/hya-tool/src/tool.rs:814-944](../../crates/hya-tool/src/tool.rs#L814-L944),
[crates/hya-tool/tests/glob_grep.rs:164-187](../../crates/hya-tool/tests/glob_grep.rs#L164-L187))

Files are sorted lexically, decoded with `tokio::fs::read_to_string`, and
searched line by line. Files that cannot be decoded/read are silently skipped.
Collection stops immediately at 100 matches, so GREP does not calculate the
actual total beyond the cap.
([crates/hya-tool/src/tool.rs:814-944](../../crates/hya-tool/src/tool.rs#L814-L944))

The result contains the regex as `title`, match-count/truncation metadata,
grouped human-readable output, structured `{file, line, text}` matches, and a
`total` equal to the number returned. No matches returns `No files found`.
As with GLOB, `truncated` becomes true at exactly 100 rows, which signals only
that collection reached the cap.
([crates/hya-tool/src/tool.rs:814-944](../../crates/hya-tool/src/tool.rs#L814-L944),
[crates/hya-tool/tests/glob_grep.rs:131-162](../../crates/hya-tool/tests/glob_grep.rs#L131-L162))

## Permissions and execution

The registry attaches invocation-level permission metadata to every canonical
name. READ, LS, GLOB, FIND, GREP, LSP, SKILL, LIST_AGENTS, ROSTER, and CHANNELS
are `ReadOnly`; TASK is `Task`; SHELL and BASH are `Command`; other builtins are
general `Tool` calls. Read-only/task invocations default to allow, general tool
invocations default to ask, command invocations extract the command string,
and MCP invocations use an MCP subject.
([crates/hya-tool/src/tool.rs:189-196](../../crates/hya-tool/src/tool.rs#L189-L196),
[crates/hya-tool/src/tool.rs:626-634](../../crates/hya-tool/src/tool.rs#L626-L634),
[crates/hya-tool/tests/tool.rs:148-188](../../crates/hya-tool/tests/tool.rs#L148-L188))

At normal app startup, the action-level snapshot explicitly allows READ, GLOB,
and GREP. Tools still make their own typed action/resource assertions. An
invocation grant satisfies later action checks except `ExternalDirectory`,
which remains independently enforceable under `default`/`strict`. Under
`permission.model: allow`, resource checks (including `ExternalDirectory`)
auto-approve unless a snapshot rule explicitly denies them; `danger` bypasses
checks entirely (including Deny).
([crates/hya-app/src/runtime.rs:3917-3940](../../crates/hya-app/src/runtime.rs#L3917-L3940),
[crates/hya-tool/src/permission.rs:539-608](../../crates/hya-tool/src/permission.rs#L539-L608))

The engine processes each model tool call by running plugin before-hooks,
resolving canonical names or aliases, authorizing the invocation, constructing
`ToolCtx`, executing the tool, and running after-hooks. Permission errors cannot
be rewritten by an after-hook. Success becomes `Event::ToolResult` (after
`cap_tool_output`); failure becomes `Event::ToolError` with a structured error
value and display message.
([crates/hya-core/src/engine/turn.rs:157-200](../../crates/hya-core/src/engine/turn.rs#L157-L200))

Tool errors are serialized as `{"error":{"type":...,"message":...}}` with these
wire `type` strings
([crates/hya-tool/src/tool.rs:40-88](../../crates/hya-tool/src/tool.rs#L40-L88),
[crates/hya-core/src/engine/tool_error.rs:4-32](../../crates/hya-core/src/engine/tool_error.rs#L4-L32)):

| Variant | Wire `type` |
| --- | --- |
| `Input` | `input` |
| `Permission` | `permission` |
| `Io` | `io` |
| `Json` | `json` |
| `Cancelled` | `cancelled` |
| `Overloaded` | `overloaded` |
| `OperationIdConflict` | `operation_id_conflict` |
| `OperationAlreadyHandled` | `operation_already_handled` |
| `UnknownAgentId` | `unknown_agent_id` |
| `AgentSpawnNotAllowed` | `agent_spawn_not_allowed` |
| `UnsupportedInlineAgentField` | `unsupported_inline_agent_field` |
| `Other` | `unknown` |

Only `permission` is protected from rewriting by `tool.execute.after` hooks.

## Runtime planes and extensions

All builtin schemas are registered before runtime capabilities are considered.
`ToolCtx` carries permission, interaction, spawner, mailbox, todo, skills, web
search, LSP, formatter, workdir, session, and cancellation planes/resources,
plus an immutable caller-reachable `AgentDef` roster derived from the bound
agent's `can_spawn` reachability (not a mutable agent catalog plane). The
single `BundleCatalog` authority lives on `RuntimeSnapshot` / `TurnBinding`;
application wiring does not replace an agent catalog authority.
([crates/hya-tool/src/tool.rs:40-88](../../crates/hya-tool/src/tool.rs#L40-L88),
[crates/hya-tool/src/agents.rs:11-20](../../crates/hya-tool/src/agents.rs#L11-L20),
[crates/hya-core/src/runtime_registry.rs:36-39](../../crates/hya-core/src/runtime_registry.rs#L36-L39),
[crates/hya-core/src/runtime_registry.rs:22-29](../../crates/hya-core/src/runtime_registry.rs#L22-L29),
[crates/hya-core/src/runtime_registry.rs:117-120](../../crates/hya-core/src/runtime_registry.rs#L117-L120),
[crates/hya-app/src/runtime.rs:3917-3940](../../crates/hya-app/src/runtime.rs#L3917-L3940))

A bare `SessionEngine` starts with a disconnected mailbox and default
interaction, spawner, todo, skill, websearch, formatter, and LSP planes. The
application replaces the interaction, spawner, mailbox, and formatter planes
and starts the mailbox service. Agent discovery for tools uses the immutable
per-turn `AgentDef` roster from the bound catalog's `can_spawn` set rather than
an injectable catalog plane. Consequently, registry presence alone does not
prove that a plane-backed tool can return useful data; for example, mailbox
operations report that they are available only inside a running team, and LSP
reports when no server supports a file type.
([crates/hya-core/src/engine.rs:258-280](../../crates/hya-core/src/engine.rs#L258-L280),
[crates/hya-app/src/runtime.rs:3917-3940](../../crates/hya-app/src/runtime.rs#L3917-L3940),
[crates/hya-app/src/runtime.rs:3917-3940](../../crates/hya-app/src/runtime.rs#L3917-L3940),
[crates/hya-tool/src/mailbox.rs:243-248](../../crates/hya-tool/src/mailbox.rs#L243-L248),
[crates/hya-tool/src/lsp.rs:15-26](../../crates/hya-tool/src/lsp.rs#L15-L26))

### MCP tools

At startup or through the Compat MCP control routes, hya prepares enabled MCP
servers and adapts tools returned by `tools/list`. Disabled or failed servers
contribute no new tools. A complete current-revision candidate is published for
the next turn; an older bound turn keeps its retained source client and view.
Only MCP tools whose input schema has `type: "object"` are accepted.
([crates/hya-mcp/src/manager.rs:59-100](../../crates/hya-mcp/src/manager.rs#L59-L100),
[crates/hya-mcp/src/manager.rs:105-140](../../crates/hya-mcp/src/manager.rs#L105-L140),
[crates/hya-mcp/src/bridge.rs:20-43](../../crates/hya-mcp/src/bridge.rs#L20-L43))

The model-facing name is `mcp__{server}__{tool}`, while execution sends the
remote tool's original name in `tools/call`. MCP adapters assert `Action::Mcp`
and are registered with `ToolPermission::Mcp`. Text and supported image/PDF
content is normalized into hya output and attachments.
([crates/hya-mcp/src/bridge.rs:36-80](../../crates/hya-mcp/src/bridge.rs#L36-L80),
[crates/hya-mcp/src/bridge.rs:83-103](../../crates/hya-mcp/src/bridge.rs#L83-L103),
[crates/hya-app/src/runtime.rs:3917-3940](../../crates/hya-app/src/runtime.rs#L3917-L3940))

### Plugin tools

Connected plugins contribute declared tools whose input schema has
`type: "object"`. Plugin tool names are preserved as declared rather than
namespaced, execution requires a session, and calls are forwarded to the
owning plugin. They are registered as general `ToolPermission::Tool` tools.
([crates/hya-plugin/src/plugin_tool.rs:18-34](../../crates/hya-plugin/src/plugin_tool.rs#L18-L34),
[crates/hya-plugin/src/plugin_tool.rs:36-58](../../crates/hya-plugin/src/plugin_tool.rs#L36-L58),
[crates/hya-plugin/src/host.rs:387-397](../../crates/hya-plugin/src/host.rs#L387-L397),
[crates/hya-app/src/runtime.rs:3917-3940](../../crates/hya-app/src/runtime.rs#L3917-L3940))

Registry names are unique across builtins, MCP tools, plugin tools, and their
aliases. Any duplicate source, configured/handshake plugin-ID mismatch,
same-source duplicate export, or canonical/alias collision rejects the whole
candidate before generation allocation; the previous effective snapshot stays
active. There is no insertion-order overwrite.
MCP namespacing reduces MCP collisions, while unnamespaced plugin declarations
can collide directly with a builtin or another plugin.
([crates/hya-tool/src/tool.rs:313-347](../../crates/hya-tool/src/tool.rs#L313-L347),
[crates/hya-app/src/runtime.rs:3917-3940](../../crates/hya-app/src/runtime.rs#L3917-L3940))

## Verified boundaries

The strongest executable contracts for the focused tools are:

- READ file/directory/media/path/permission behavior:
  [crates/hya-tool/tests/read.rs:60-267](../../crates/hya-tool/tests/read.rs#L60-L267)
- READ text limits:
  [crates/hya-tool/tests/read_limits.rs:56-117](../../crates/hya-tool/tests/read_limits.rs#L56-L117)
- READ missing-path suggestions:
  [crates/hya-tool/tests/read_missing.rs:56-77](../../crates/hya-tool/tests/read_missing.rs#L56-L77)
- EDIT permissions, BOM/line endings, formatter, and result metadata:
  [crates/hya-tool/tests/edit.rs:80-238](../../crates/hya-tool/tests/edit.rs#L80-L238)
- EDIT fuzzy matching:
  [crates/hya-tool/tests/edit_fuzzy.rs:56-275](../../crates/hya-tool/tests/edit_fuzzy.rs#L56-L275)
- GLOB/GREP path, output, include, permission, and file-path widening:
  [crates/hya-tool/tests/glob_grep.rs:83-214](../../crates/hya-tool/tests/glob_grep.rs#L83-L214)
- Canonical names, hidden aliases, and registry permission metadata:
  [crates/hya-tool/tests/tool.rs:111-188](../../crates/hya-tool/tests/tool.rs#L111-L188)

The source-derived edge cases called out above are intentional descriptions of
current behavior, not broader compatibility guarantees. In particular, tests
do not currently pin GLOB/GREP behavior at exactly 100 rows, FIND's lack of an
external-directory check, or READ's partial `totalLines` value after the byte
cap. Those observations should be rechecked if the corresponding collectors or
permission paths change.
