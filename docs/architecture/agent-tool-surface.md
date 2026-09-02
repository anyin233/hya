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

`ToolRegistry::builtins()` installs **27** canonical schema names before model
filtering. The inventory below is complete for that constructor. A schema marked
**closed** rejects unknown keys; aliases resolve only at runtime and are never
advertised.
([crates/hya-tool/src/tool.rs](../../crates/hya-tool/src/tool.rs))

| Area | Canonical schema names | Role |
| --- | --- | --- |
| File access | `read`, `write`, `edit`, `apply_patch` | Read or mutate workspace files. `read`, `write`, and `edit` are native coding tools; `apply_patch` remains the separate patch envelope. |
| Local discovery | `ls`, `glob`, `find`, `grep`, `lsp` | List directories, match paths, search text, or query language servers. |
| Commands | `bash` | Run a command with bounded capture; hidden runtime name `shell` is not advertised. |
| Human/session interaction | `question`, `ask_user`, `todowrite`, `plan_exit`, `invalid` | Ask structured or simple questions, update session todos, request a plan-mode transition, or represent invalid tool arguments. |
| Agents and teams | `skill`, `list_agents`, `task`, `workflow`, `send`, `announce`, `roster`, `channels`, `join`, `leave` | Load skills, discover/spawn agents, execute governed Workflow commands, and use unit mail/channels ([ADR-0011](../adr/0011-hierarchy-scoped-mailbox.md)). |
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
| `inline_agent` | Request-scoped overlay. Published fields are `name`, `prompt`, `category`, `model`, and `resident`; nested `description` is not advertised. |
| `members[]` | hya extension: fan one call out to several subagents (each needs `prompt`; optional per-member overrides). |

The nested `inline_agent.description` parser field is retained only to handle
stale/direct callers. Empty or whitespace-only values normalize to absence, so
the captured empty-description request can spawn. A non-empty value is rejected
before admission with typed wire error `unsupported_inline_agent_field`, with no
child or session side effect. This hidden compatibility does not change
authorization, model/category precedence, resident behavior, or run-tree
projection.

([crates/hya-tool/src/task.rs](../../crates/hya-tool/src/task.rs))

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

**`roster`**: no parameters; returns the acting agent's `self` path plus rows
grouped by relation — `parent`, `peers` (same parent), and `reports` (agents it
leads). Nobody outside the agent's unit is listed, because it cannot message
them ([ADR-0011](../adr/0011-hierarchy-scoped-mailbox.md)). Each row carries
`handle` (canonical path), `name` (the short name used to address it),
`relation`, agent type, session id, scheduling mode, `status` (`idle` | `busy` |
`done` | `failed`, folded in from `AgentActivityChanged` by the resident
supervisor), and `current_task`. Empty groups are omitted. Registered
`ToolPermission::ReadOnly`, so it allows without prompting under `default`.

**`announce`**: takes `body`; posts a one-way announcement to the agents the
caller **directly** leads, and no further. Rejected when the caller leads nobody.
Subordinates do not reply on this path — they answer with ordinary `send` mail to
their parent.
([crates/hya-tool/src/mailbox.rs:314-390](../../crates/hya-tool/src/mailbox.rs#L314-L390))

**`channels`**: no parameters; lists the channels the acting agent can use — its
home unit's, plus its own unit's when it leads one — with the owning `unit`,
member list, and message count. A channel belongs to exactly one unit, so the
same name in another unit is a different channel. The reserved `#announce`
channels are hidden. Registered `ToolPermission::ReadOnly`.
([crates/hya-tool/src/mailbox.rs:393-437](../../crates/hya-tool/src/mailbox.rs#L393-L437))

**`join`**: takes a channel name; the leading `#` is optional (`#build` and
`build` are the same channel). It subscribes the acting agent **within its own
unit** and **creates** the channel if it does not exist — there is no separate
create-channel tool. An agent that leads a unit resolves a bare name to the unit
it leads, and `^name` to its parent's unit; for an agent that leads nobody a bare
name is its home unit and `^` is an error.
Registered `ToolPermission::Tool`, so it asks under `default`.
([crates/hya-tool/src/mailbox.rs:439-473](../../crates/hya-tool/src/mailbox.rs#L439-L473))

**`leave`**: takes a channel name (leading `#` optional) and unsubscribes the
acting agent. After leaving, channel posts no longer reach the agent but direct
handle mail still does. Registered `ToolPermission::Tool`.
([crates/hya-tool/src/mailbox.rs:475-504](../../crates/hya-tool/src/mailbox.rs#L475-L504))

`list_agents` enumerates definitions usable by `task`.
([crates/hya-tool/src/agents.rs:22-84](../../crates/hya-tool/src/agents.rs#L22-L84))

### Bash

`bash` is the sole model-facing command tool. Its closed base schema is:

```json
{
  "command": "string",
  "env": { "string": "string" },
  "timeout": "number (seconds)",
  "cwd": "string",
  "pty": "boolean"
}
```

Only `command` is required. The default timeout is 300 seconds; `timeout: 0`
disables the deadline, and other finite values clamp to 1..=3600 seconds with a
clamp notice. `cwd` is checked against the existing lexical workdir policy.
Command permission is checked before process creation. Timeout and cancellation
terminate and reap the complete process group. Non-PTY stdout/stderr are
captured concurrently in arrival order; PTY mode uses a real PTY and keeps
observing the deadline/cancellation after leader exit while descendants retain
the slave. Inline output is capped at 50 KiB after timeout/clamp notices are
added. A truncated result points to the complete raw stream in a private
mode-0600 hya artifact; an armed owner removes partial/unpublished artifacts on
every other exit. Nonzero exits and timeouts are completed structured results
with status metadata, while explicit cancellation is typed `cancelled`.
Environment values are never echoed in titles, output, diagnostics, metadata,
or the TUI.

The old `shell` name remains only as a hidden runtime alias for stale callers;
it is not an advertised schema and uses the same implementation. This is
intentional compatibility, not a second command surface.
([crates/hya-tool/src/shell.rs](../../crates/hya-tool/src/shell.rs),
[crates/hya-tool/src/tool.rs](../../crates/hya-tool/src/tool.rs))

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

Six legacy aliases resolve during execution but do not appear in
`ToolRegistry::schemas()`:

| Canonical advertised name | Hidden lookup alias |
| --- | --- |
| `bash` | `shell` |
| `webfetch` | `fetch` |
| `websearch` | `search` |
| `todowrite` | `todo` |
| `apply_patch` | `patch` |
| `plan_exit` | `plan` |

The `shell` entry is the only compatibility spelling for the command tool; it
uses the canonical Bash schema and permission path. The other aliases are
existing registry conveniences. All aliases remain non-advertised and never
change a canonical schema's input fields.
([`crates/hya-tool/src/tool.rs`](../../crates/hya-tool/src/tool.rs))

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

### Canonical schema and path compatibility

The advertised schema is closed and requires only `path`:

```json
{
  "path": "string",
  "offset": "integer >= 1",
  "limit": "integer >= 1",
  "raw": "boolean"
}
```

`filePath` is parsed only as a hidden compatibility field for pre-0.36.9
requests. Runtime resolution keeps `path` and `filePath` distinct: one non-empty
spelling succeeds, equal non-empty values succeed, conflicting non-empty values
return a typed input error, and both missing/empty values fail. Paths are not
trimmed. A legacy `offset: 0` maps to line 1; zero is not advertised.

The workdir is absolutized and lexically normalized. Relative paths join the
workdir; absolute paths remain absolute. `.` is removed and `..` pops one text
component. Symlinks are not canonicalized for the external-directory check, so
a symlink inside the workdir is not external solely because its target is
outside. A lexically external Read authorizes one kind-blind parent wildcard
before metadata, existence, target-kind, normal Read permission, or missing-path
details are observed.

### File kinds and text output

Read dispatches directories, supported media, and text. PNG, JPEG, GIF, WebP,
and PDF files return bounded base64 data-URL attachments. Unsupported binary
files return a typed error. Directory reads list immediate children, put
directories first, sort each group lexically, and use one-based offset paging.

Text removes one leading UTF-8 BOM and normalizes CRLF/lone CR to LF. Non-raw
output uses contextual hashline rows (`LINE#HASH:content`) with stable line
numbers; `raw` returns normalized unanchored text. The terminal empty newline
sentinel is excluded from rendered rows. The default line limit is 2,000 and
the aggregate text budget is 50 KiB; long lines and aggregate truncation carry
bounded notices and a continuation `nextOffset`. Every output also carries
bounded display metadata (`type`, `path`, `text`, `lineStart`, `lineEnd`,
`totalLines`, and `truncated`) for the TUI. Invalid UTF-8 is replaced with
U+FFFD and reported as a warning rather than silently omitted.

([crates/hya-tool/src/read.rs](../../crates/hya-tool/src/read.rs),
[crates/hya-tool/src/hashline/mod.rs](../../crates/hya-tool/src/hashline/mod.rs))

## WRITE

### Canonical schema and result

Write exposes only the closed `{ "path": string, "content": string }` schema.
It keeps hya's existing lexical permission, formatter, LSP, BOM, and whole-file
semantics. Parent directories are created as needed; writes use the shared
same-directory atomic writer, preserve mode/BOM/line-ending behavior, and mark
a leading shebang executable when possible. A chmod failure is a bounded warning,
not a silent failure. Accidental hashline display prefixes are stripped only
when the complete input is unambiguously a rendered hashline block; ambiguous
content remains unchanged.

Formatter and LSP processing run before the final result is built. The returned
`output`, preview, diagnostics, and bounded display metadata therefore describe
the final post-formatter bytes, not the pre-format input. Write also updates the
shared hashline snapshot state so a later Edit can use the same recovery chain.

([crates/hya-tool/src/write.rs](../../crates/hya-tool/src/write.rs),
[crates/hya-tool/src/hashline/fs.rs](../../crates/hya-tool/src/hashline/fs.rs))

## EDIT

### Canonical schema

Edit is a strict hashline operation and exposes only the closed `path + edits`
schema. Every operation object is closed too:

```json
{
  "path": "src/main.rs",
  "edits": [
    { "op": "replace", "pos": "12#KT", "lines": ["new line"] },
    { "op": "append", "lines": ["last line"] },
    { "op": "prepend", "pos": "1#JB", "lines": ["first line"] },
    { "op": "replace_text", "oldText": "old", "newText": "new" }
  ]
}
```

`replace` accepts an optional inclusive `end` anchor and empty `lines` to
delete; `append` defaults to EOF and `prepend` defaults to BOF. `replace_text`
requires exactly one exact occurrence. Literal lines must be file content, not
copied hashline or diff prefixes. The parser rejects unknown fields, malformed
anchors, mixed operation fields, duplicate/conflicting spans, and wrong types
with stable input codes.

### Anchor validation and recovery

Each anchor is validated against the same pre-edit normalized snapshot. Text
hints can disambiguate a hash collision; hashes are stale-reference aids, never
integrity or authorization data. All spans are resolved before any mutation,
then applied bottom-up. The runtime rejects edits that would make a non-empty
file byte-empty and guards repeated successful payloads and no-op loops.

Only a direct `E_STALE_ANCHOR` failure enters recovery. Stored snapshots are
tried newest-first, and each candidate is merged onto live content with exact
context-three, fuzz-zero hunks. The first exact merge wins; conflicts retain
the stale error plus a recovery note. No fuzzy relocation is attempted. Fresh
anchors, diff metadata, diagnostics, and snapshots are generated from final
post-formatter bytes. The prepared target identity is revalidated immediately
before ordinary rename or hard-link truncate/open, so pathname/alias swaps fail
before mutation. Formatter/LSP failure after mutation reports that the file
changed while retaining the authoritative final snapshot.

The runtime is process-local and bounded by Session, workdir, and resolved
target. It retains at most eight targets, four versions per target, and 32 MiB
of snapshot text. A fixed lock-shard array serializes same-target mutations;
hard-linked aliases share filesystem identity. Cancellation while waiting for
the lock or during execution returns typed `cancelled` without exposing file
contents. After a commit, cancellation first reconciles the actual bytes,
snapshot, and duplicate guard, then returns the typed cancellation.

([crates/hya-tool/src/edit.rs](../../crates/hya-tool/src/edit.rs),
[crates/hya-tool/src/hashline/apply.rs](../../crates/hya-tool/src/hashline/apply.rs),
[crates/hya-tool/src/hashline/merge.rs](../../crates/hya-tool/src/hashline/merge.rs),
[crates/hya-tool/src/hashline/state.rs](../../crates/hya-tool/src/hashline/state.rs))

## GREP

### Canonical schema and search behavior

Grep is native Rust and does not invoke `rg`. Its closed schema requires
`pattern` and accepts only `path`, `glob`, `ignoreCase`, `literal`, `context`
(0..=5), and `limit` (1..=200) as optional fields:

```json
{
  "pattern": "TODO|FIXME",
  "path": "src",
  "glob": "*.rs",
  "ignoreCase": true,
  "literal": false,
  "context": 2,
  "limit": 50
}
```

Traversal is cancellable inside directory walking, ignore parsing, line discard,
and matching. It is gitignore-aware, deterministic, and permission-checked for
the search root before metadata probing. Caller globs stop at 4,096 bytes and
both `[!x]` and `[^x]` mean class negation in caller and ignore patterns. Ignore
sources/rule counts are bounded. A logical line over 1 MiB is discarded through
its newline with one bounded warning, then later normal lines remain searchable.
Regex and literal modes honor `ignoreCase`; context ranges are merged and
separated in the result. The worker reads one extra match before setting
`truncated`, so `limit` and `limit + 1` are distinguishable. Matched files are
loaded through the shared text/hashline path, which records snapshots only for
successfully rendered text.

The result keeps the bounded model summary and adds
`metadata.display.groups[]`, where each group is `{path, rows[]}` and each row
contains `{line, text, isMatch}`. Per-file rows are numbered and carry enough
metadata for syntax-aware TUI rendering without changing the durable Event
model. Grep snapshots enable the same exact stale-anchor recovery path as Read
and Edit.

([crates/hya-tool/src/hashline/mod.rs](../../crates/hya-tool/src/hashline/mod.rs),
[crates/hya-tool/src/grep.rs](../../crates/hya-tool/src/grep.rs))

## APPLY_PATCH

The parameter is `patchText` (serde alias `patch`) carrying a Codex/Compat-style
patch envelope. Supported hunk kinds are **add**, **update**, **delete**, and
**move** (move is an update header plus an optional move line).
([crates/hya-tool/src/apply_patch/mod.rs:16-55](../../crates/hya-tool/src/apply_patch/mod.rs#L16-L55),
[crates/hya-tool/src/apply_patch/parse.rs](../../crates/hya-tool/src/apply_patch/parse.rs),
[crates/hya-tool/src/apply_patch/apply.rs:59-121](../../crates/hya-tool/src/apply_patch/apply.rs#L59-L121))

### Patch envelope grammar

Parsed by `parse_patch` after CRLF → LF normalization:

1. **Sentinels (required):** a line whose trim is exactly `*** Begin Patch`, then
   later a line whose trim is exactly `*** End Patch`. Begin must precede End;
   missing either is an input error (`invalid patch format: …`).
2. **Between the sentinels**, file operations are introduced by headers:
   - `*** Add File: <path>` — body lines until the next file header must **each**
     start with `+` (any other prefix → `add file lines must start with '+'`).
     The `+` is stripped; lines are joined with `\n` and a trailing newline when
     non-empty.
   - `*** Delete File: <path>` — no body.
   - `*** Update File: <path>` — optionally the **very next** line may be
     `*** Move to: <path>` (move destination). Then zero or more update chunks.
3. **Update chunks** start with a line beginning `@@` (optional trailing context
   text after `@@`). Chunk body lines use a one-character prefix:
   - leading space — context (present in both old and new)
   - `-` — removed line
   - `+` — added line  
   A line exactly `*** End of File` ends the chunk and marks end-of-file matching.
4. **Empty / unrecognized body:** if no recognized headers appear between Begin
   and End, the parser returns zero hunks; the tool then rejects with
   `patch rejected: empty patch`.

Example:

```text
*** Begin Patch
*** Add File: notes/hello.txt
+hello
*** Update File: src/main.rs
@@ fn main
-old
+new
*** Delete File: obsolete.txt
*** End Patch
```

Every path in the envelope must be relative and must not escape the session
workdir: an absolute path or a `..` component is an **input error**. Every
touched path (and move destination) is permission-checked as `Action::Edit`
**before** any file is written, so a denial leaves the whole patch unapplied.
([crates/hya-tool/src/apply_patch/mod.rs](../../crates/hya-tool/src/apply_patch/mod.rs))

The result is a Compat-style title plus an aggregate diff and per-file metadata.
After application, the same post-edit formatter + BOM re-sync + LSP-diagnostics
step as write/edit runs for non-delete paths.

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

GLOB, FIND, and GREP use native Rust traversal. GREP does not invoke `rg` or
another external process. Search workers are deterministic and retain only
bounded rows/metadata. GLOB and GREP check cancellation inside traversal work,
not only between completed files. Relative roots resolve against the session
workdir where the tool contract requires it; one kind-blind lexical external
resource is authorized before metadata or target-kind probing.

### GLOB and FIND

GLOB requires `pattern` and optionally accepts a directory `path` (defaulting to
the workdir). Caller patterns stop at 4,096 bytes; `[!x]` and `[^x]` both express
class negation. It returns lexically sorted file paths, capped at 100 rows, with
count and truncation metadata. FIND retains its compatibility-oriented
`{path,size}` result shape and existing unbounded positive result behavior.

### GREP

Grep requires a non-empty `pattern` and accepts only these optional fields:
`path`, `glob`, `ignoreCase`, `literal`, `context` (0..=5), and `limit` (1..=200).
The input object is closed and rejects unspecified keys.
Regex and literal matching honor `ignoreCase`. Caller and ignore patterns share
negated-class semantics. Traversal bounds ignore sources/rules, skips an
over-budget ignore file with one bounded warning, and continues without
retaining its rules. A file is streamed by logical line; after 1 MiB the worker
discards bytes through the newline without growing the retained line, warns
once, and continues so later matches remain visible. Unsupported/unreadable
files are skipped without exposing their contents, and deterministic file/match
order is preserved.

Context ranges merge when adjacent or overlapping and use separators between
disjoint ranges. Collection observes one additional match before setting
`truncated`, so an exact limit is distinct from `limit + 1`. Matched files are
loaded through the shared text normalizer and hashline formatter; successful
loads update the same Session/workdir/target snapshot history used by Read and
Edit, enabling exact stale-anchor recovery.

The result contains a bounded summary plus
`metadata.display.groups[]`. Each group has a file `path` and bounded `rows`
with `{line, text, isMatch}`. This metadata is a presentation hint, not a new
event or read-model store. It lets the TUI render a titled block per file with
file-derived syntax highlighting while keeping match identity visible.

([crates/hya-tool/src/grep.rs](../../crates/hya-tool/src/grep.rs),
[crates/hya-tool/src/hashline/mod.rs](../../crates/hya-tool/src/hashline/mod.rs))

## Result envelope and presentation boundary

Every completed builtin coding-tool result keeps the shared `{title, output,
metadata}` envelope. `output` is bounded model-facing text; `metadata` is a
bounded host-facing semantic payload. Read, Write, and Grep expose only bounded
line/file facts, Edit may carry a separately bounded diff and diagnostics, and
Bash exposes bounded command/output status plus truncation and artifact metadata.
The structural cap serializes each nested row/group exactly once and is
idempotent. The engine reapplies it after post-tool hooks, immediately before
durable publication, so neither metadata nor a hook bypasses the final bound.
Provider replay prefers the string `output` member; an object without that
member falls back to serialized JSON.

The hya TypeScript UI consumes projected SDK `ToolPart` state only. SyncProvider
owns initial hydration and live replacement; presentation does not fetch, poll,
replay Events, hydrate a second message store, or schedule a timer. Completed
parts use one allowlisted presentation boundary:

| Tool | Completed presentation |
| --- | --- |
| Read | Titled file/directory block with file-derived syntax highlighting, stable line numbers/offsets, authoritative truncation flags, and bounded collapse. Attachments and directories keep their existing readable fallback. |
| Write | Titled file block with final post-formatter text, syntax highlighting, stable line numbers, first three positioned severity-one diagnostics, and bounded collapse. |
| Edit | Existing semantic diff primitive, with final-state metadata, first three positioned severity-one diagnostics, and distinct narrow unified rows. |
| Grep | One titled block per matched file, numbered context/match rows, explicit match identity, authoritative group/row truncation, and file-derived highlighting. |
| Bash / hidden Shell | One command/output block: nullable exit remains valid for timeout/signal, only the command is syntax-highlighted, output is plain ANSI-stripped text, and textual exit/timeout/truncation status remains. `env` and unknown input keys are excluded. |

Pending, streaming, permission, denied, malformed-data, attachment, directory,
diagnostic, error, and generic fallback states remain on their existing paths.
Malformed or compacted metadata returns to the readable inline/error fallback;
it never renders arbitrary input keys. Local expand/collapse is reversible UI
state and is not persisted as a new Event. At 80 columns Edit uses unified
layout and keeps removed/added rows separate; wide terminals may use split
layout above 120 columns. Replaying a Session through the same SDK projection
produces the same completed blocks.

([packages/hya-tui-ts/src/hya/coding-tool-presentation.tsx](../../packages/hya-tui-ts/src/hya/coding-tool-presentation.tsx))

## Permissions and execution


The registry attaches invocation-level permission metadata to every canonical
name. READ, LS, GLOB, FIND, GREP, LSP, SKILL, LIST_AGENTS, ROSTER, and CHANNELS
are `ReadOnly`; TASK is `Task`; BASH is `Command`; other builtins are general
`Tool` calls. The hidden `shell` alias resolves to the same Bash tool and uses
the same command permission subject. Read-only/task invocations default to
allow, general tool invocations default to ask, and command invocations include
the full command string.
([crates/hya-tool/src/tool.rs](../../crates/hya-tool/src/tool.rs))

Coding-tool permission order is stable: invocation admission runs first. Read
and Grep derive the containing lexical `<dir>/*` scope and authorize it before
metadata/existence/target-kind probing; denied file and directory siblings use
the same resource. Tool-specific permission follows, then filesystem work.
Bash checks command permission before process creation and checks
`ExternalDirectory` for an outside `cwd`. Paths are absolutized and lexically
normalized without symlink canonicalization, preserving the existing symlink
policy. A call-scoped invocation grant never satisfies the separate
external-directory check.

At normal app startup, the action-level snapshot explicitly allows READ, GLOB,
and GREP. Tools still make their own typed action/resource assertions. An
invocation grant satisfies later action checks except `ExternalDirectory`,
which remains independently enforceable under `default`/`strict`. Under
`permission.model: allow`, resource checks (including `ExternalDirectory`)
auto-approve unless a snapshot rule explicitly denies them; `danger` bypasses
checks entirely (including Deny).

The engine processes each model tool call by running plugin before-hooks,
resolving canonical names or hidden aliases, authorizing the invocation,
constructing `ToolCtx`, executing the tool, and running after-hooks. Permission
errors cannot be rewritten by an after-hook. Success is capped before hooks and
again after the final hook replacement, then becomes `Event::ToolResult`;
failure becomes `Event::ToolError` with a structured error value and display
message. Unknown tools and malformed input fail before permission asks. All
coding-tool cancellation paths preserve the typed `cancelled` error.

Tool errors are serialized as `{"error":{"type":...,"message":...}}` with
these wire `type` strings:

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

## Provenance

Read, Edit, and Grep behavior follows `pi-hashline-edit` 0.8.3, pinned by npm
git head `ba7db9943d0f58499b24c1f6bd64722580f772a5` and tarball SHA-1
`8985f24c3493be375cc225a5522ed54de8daabc9`. Write and Bash are host contracts
aligned with `@oh-my-pi/pi-coding-agent` 18.1.3 at
`can1357/oh-my-pi@0b769cc4dd9771373335430385d1d2f696dc3498`. The Rust
implementation is native and does not add a JavaScript runtime dependency;
license notices are shipped with the source-derived implementation.

## Verified boundaries

The focused contracts are owned by these seams:

- Native schemas and adapters: [`crates/hya-tool/src/read.rs`](../../crates/hya-tool/src/read.rs), [`write.rs`](../../crates/hya-tool/src/write.rs), [`edit.rs`](../../crates/hya-tool/src/edit.rs), [`grep.rs`](../../crates/hya-tool/src/grep.rs), and [`shell.rs`](../../crates/hya-tool/src/shell.rs).
- Shared hashline formatting, strict operations, exact recovery, atomic writes,
  snapshots, and lock bounds: [`crates/hya-tool/src/hashline/`](../../crates/hya-tool/src/hashline/).
- Invocation/resource permission and typed error mapping:
  [`crates/hya-tool/src/permission.rs`](../../crates/hya-tool/src/permission.rs)
  and [`crates/hya-core/src/engine/tool_error.rs`](../../crates/hya-core/src/engine/tool_error.rs).
- Durable result projection and provider replay: [`docs/architecture/event-model.md`](event-model.md).
- Hya-owned completed coding-tool views:
  [`packages/hya-tui-ts/src/hya/coding-tool-presentation.tsx`](../../packages/hya-tui-ts/src/hya/coding-tool-presentation.tsx).

These boundaries describe shipped behavior, not a promise of full Compat
superset behavior. Historical 0.36.8 `ToolError` Events remain immutable and
visible on replay. An already-running 0.36.8 backend must restart before new
calls use the 0.36.9 schemas and runtime.
