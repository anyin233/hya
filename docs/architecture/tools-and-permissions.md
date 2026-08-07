# Tools and Permissions

The tool system lives in [`../../crates/hya-tool`](../../crates/hya-tool). The
engine exposes tool schemas to the model, then executes requested calls only
after permission checks pass.

## Tool Registry

[`tool.rs`](../../crates/hya-tool/src/tool.rs) defines:

- `Tool`: name, schema, async execute.
- `ToolCtx`: permission plane, interaction/spawner/todo/skill/websearch/LSP/
  formatter planes, session ids, workdir, cancellation token.
- `ToolRegistry`: name-to-tool map, aliases, and model-facing schemas.

`ToolRegistry::builtins()` installs **26** canonical schema names before model
filtering
([`tool.rs:311-347`](../../crates/hya-tool/src/tool.rs#L311-L347)). The table
below is the complete inventory. Advertised field names are the model-facing
JSON schema `required`/`properties` keys. Short spellings such as `path` for
`filePath` (and `old`/`new`/`replace_all` for edit) are listed under schema
`properties` for compatibility; they are usually absent from `required`, which
still names the camelCase fields.

| Tool | Input (advertised) | Output |
| --- | --- | --- |
| `invalid` | unknown call payload | Structured invalid-tool response. |
| `read` | `{ "filePath": string, "offset"?: number, "limit"?: number }` (also lists `path` under `properties`) | File text/media or directory listing. |
| `write` | `{ "filePath": string, "content": string }` (also lists `path` under `properties`) | Write result plus formatter/LSP diagnostics when available. |
| `edit` | `{ "filePath": string, "oldString": string, "newString": string, "replaceAll"?: bool }` (also lists `path`/`old`/`new`/`replace_all` under `properties`) | Replacement result plus diff/formatter/LSP data. |
| `apply_patch` (`patch`) | `{ "patchText": string }` (alias `patch`) | Aggregate diff and per-file metadata. |
| `ls` | `{ "path"?: string }` | Immediate directory entries. |
| `glob` | `{ "pattern": string, "path"?: string }` | Path matches and counts (cap 100). |
| `find` | `{ "pattern": string, "path"?: string }` | `{path, size}` matches (no row cap). |
| `grep` | `{ "pattern": string, "path"?: string, "include"?: string }` | Regex matches and counts (cap 100). |
| `shell`, `bash` | `{ "command": string, "timeout"?: number, "workdir"?: string, "env"?: object }` | Command title, stdout/stderr, exit status. |
| `webfetch` (`fetch`) | `{ "url": string, "format"?: "text"\|"markdown"\|"html", "timeout"?: number }` | Fetched web content or image attachment. |
| `websearch` (`search`) | `{ "query": string, "numResults"?: number, "livecrawl"?: string, "type"?: string, "contextMaxCharacters"?: number }` | Search results from the configured `WebSearchPlane`. |
| `question` | `{ "questions": [{ "question", "header", "options", "multiple"?, "custom"? }] }` | Chosen option labels (unanswered → `Unanswered`). |
| `ask_user` | `{ "question": string, "kind"?: "text"\|"select", "options"?, "allow_custom"?, "default"? }` | Answer object; cancellation returns `{ "answer": "", "cancelled": true }`. |
| `lsp` | `{ "operation", "filePath", "line", "character", "query"? }` | LSP provider response. |
| `skill` | `{ "name": string }` (name only; a path is not accepted) | `<skill_content>` envelope with body, `file://` base dir, and sampled files (cap 10). See also [`docs/skills.md`](../skills.md). |
| `list_agents` | (none) | Agent definitions usable by `task`. |
| `task` | `{ "description", "prompt", "subagent_type"?, "category"?, "model"?, "task_id"?, "command"?, "background"?, "resident"?, "inline_agent"?, "members"?: [...] }` | Foreground/background subagent outcomes. |
| `todowrite` (`todo`) | `{ "todos": [{ "content", "status", "priority" }] }` | Latest todo snapshot for the session (replace, not append). |
| `plan_exit` (`plan`) | plan status input | Plan-mode completion signal. |
| `send` | `{ "to": string, "body": string, "kind"?: "message"\|"announcement" }` | Mail delivery receipt. |
| `roster` | (none) | Live teammates with handle, type, status, task. |
| `channels` | (none) | Team channels with members and message counts. |
| `join` | `{ "channel": string }` | Subscribe (creates channel if missing). |
| `leave` | `{ "channel": string }` | Unsubscribe from a channel. |

Hidden aliases resolve at execution but are not advertised: `fetch`→`webfetch`,
`search`→`websearch`, `todo`→`todowrite`, `patch`→`apply_patch`, `plan`→`plan_exit`.

## Output Limits

Two stacked caps apply. Per-tool caps run inside the tool; a global cap runs
afterward on every successful result.

### Per-tool caps

- **`shell` / `bash`**: combined stdout+stderr is capped at **16 KiB**
  (`MAX_OUTPUT_BYTES = 16 * 1024`). Oversized output is truncated for the model
  and the full text is spilled under `.hya/tool-output/` in the session workdir
  ([`shell.rs`](../../crates/hya-tool/src/shell.rs)).
- **`glob` / `grep`**: returned rows are capped at **`SEARCH_LIMIT = 100`**
  ([`tool.rs`](../../crates/hya-tool/src/tool.rs)). Counts and truncation
  metadata remain on the result. `find` has no row cap.

### Global cap (`cap_tool_output`)

After a successful builtin, MCP, or plugin call, the engine passes the result
through `hya_tool::cap_tool_output` before emitting `Event::ToolResult`
([`turn.rs`](../../crates/hya-core/src/engine/turn.rs)). The constant is
`MAX_TOOL_OUTPUT_CHARS = 5000` characters
([`output_cap.rs`](../../crates/hya-tool/src/output_cap.rs)):

- Under the limit, the original JSON `Value` shape is preserved.
- Over the limit, the value becomes a **plain string** prefixed with
  `[tool output truncated: original N chars; showing last 5000 chars]`, followed
  by only the **last** 5000 characters of the display text.

Downstream consumers (projection, TUI, next model round) must tolerate that a
structured JSON tool result can collapse to a string after the global cap.

## Permission Models

[`permission.rs`](../../crates/hya-tool/src/permission.rs) defines:

| Type | Meaning |
| --- | --- |
| `InvocationPolicy` | Compiled ordered regex rules and the active invocation model. |
| `Invocation` | Canonical tool, MCP, and post-hook command subjects for one call. |
| `Action` | Resource operation category (fourteen values; see below). |
| `Resource` | Permission object (nine shapes; see below). |
| `Mode` | `Allow`, `Ask`, or `Deny`. |
| `Rule` | Action + resource pattern + mode. |
| `Decision` | User or interceptor response: allow once, allow always, or reject with optional feedback. |
| `PermissionPlane` | Invocation policy, resource rules, remembered grants, optional interceptor, and ask channel. |
| `PermissionInterceptor` | Optional async hook consulted after remembered grants and before the user ask. |

### Action (fourteen values)

`Action` serializes with `#[serde(rename_all = "lowercase")]` in saved-permission
rows and rules
([`permission.rs`](../../crates/hya-tool/src/permission.rs)): the variant name
is lowercased **without** inserting separators, so multi-word variants become a
single token (for example `ExternalDirectory` → `externaldirectory`). Server
persistence writes that serde string into the DB `action` column
([`saved_permission.rs`](../../crates/hya-server/src/pending/saved_permission.rs)).

| Wire value | Variant | Typical raisers |
| --- | --- | --- |
| `tool` | `Tool` | Invocation-level native tool subjects (`PermissionTarget::Tool`). |
| `read` | `Read` | `read`, `ls`. |
| `edit` | `Edit` | `write`, `edit`, `apply_patch`. |
| `glob` | `Glob` | `glob`, `find`. |
| `grep` | `Grep` | `grep`. |
| `bash` | `Bash` | `shell`, `bash` (and invocation command subjects). |
| `task` | `Task` | `task` (per member / subagent type). |
| `mcp` | `Mcp` | MCP bridge tools (`mcp__…`). |
| `webfetch` | `WebFetch` | `webfetch`. |
| `websearch` | `WebSearch` | `websearch`. |
| `todowrite` | `TodoWrite` | `todowrite`. |
| `skill` | `Skill` | `skill`. |
| `lsp` | `Lsp` | `lsp`. |
| `externaldirectory` | `ExternalDirectory` | Any tool whose resolved path (or shell `workdir`) lies outside the session workdir. |

### Resource (nine shapes)

| Shape | Payload | Notes |
| --- | --- | --- |
| `Tool(name)` | Tool name | Invocation-level tool subject. |
| `Path(resolved path)` | Absolute or display path | File/directory resource checks. |
| `Glob(pattern)` | Glob or grep pattern string | Used by `glob`/`find`/`grep` resource asserts. |
| `Command(text)` | Shell command text, **or** the namespaced MCP tool name | Shared by bash and MCP subjects. |
| `Subagent(agent id)` | Subagent type / agent id | `task` members. |
| `Url(url)` | Fetched URL | `webfetch`. |
| `WebSearch(query)` | Search query | `websearch`. |
| `Skill(name)` | Skill name | `skill`. |
| `Any` | — | Matches everything at the resource layer. |

Every resource flattens to a single match-pattern string via `Resource::pattern()`.
`Any` flattens to `"*"`. That is why a resource-level **allow always** grant
stores `Rule(action, "*", Allow)` and then allows the entire action
([`apply_decision`](../../crates/hya-tool/src/permission.rs)).

The plugin wire form of the same union is `WireResource`: tagged variants
`tool`, `path`, `glob`, `command`, `subagent`, `url`, `web_search`, `skill`,
and `any`
([`messages.rs`](../../crates/hya-plugin/src/messages.rs)).

Invocation rules are Rust regular expressions over explicitly registered
metadata. Normal built-ins and plugins expose their canonical `tool` name, MCP
tools expose only their namespaced `mcp` name, and shell tools expose both their
canonical tool name and the full command after before-hooks. Registry metadata,
not a name-prefix check, determines which domain applies.

The invocation evaluator runs once before execution. `default` uses its last
matching rule and classification fallback; `allow` permits unless a deny
matches; `strict` asks unless a deny matches or an exact remembered grant
exists; `danger` bypasses invocation and legacy checks. A successful invocation
authorization creates a call-scoped plane so a tool's internal resource check
does not duplicate the same prompt.

Resource rules remain a separate compatibility layer. They use the existing
small `*` wildcard matcher, preserve last-match-wins behavior, and continue to
own paths, URLs, subagent types, and the external-directory trust boundary. An
explicit resource deny remains authoritative after invocation approval.

## Ask Flow

When an action evaluates to `Ask`:

1. `PermissionPlane` checks the applicable invocation or resource rules and
   remembered grant (snapshot Allow/Deny first; then persistent allow-always
   rules; call-scoped grants do **not** satisfy `ExternalDirectory`).
2. **Interceptor stage**: if a `PermissionInterceptor` is installed via
   `PermissionPlane::with_interceptor`, it runs **after** remembered grants and
   **before** the user ask channel, at both the invocation gate
   (`authorize`) and the resource gate (`assert`). Returning `Some(Decision)`
   short-circuits the prompt; returning `None` defers to the normal ask channel.
   The interceptor contributes its own identity to `semantic_identity_v1`, so
   swapping interceptors changes the policy fingerprint. The only shipped
   implementation is the plugin `PermissionBridge`.
3. If still unresolved, it sends an `AskRequest` containing action, resource,
   and a reply channel.
4. The caller answers with a `Decision`.
5. `AllowOnce` permits only the current call.
6. Native invocation `AllowAlways` remembers only the selected exact target and
   value. Legacy resource `AllowAlways` continues to allow the whole action
   (`Rule(action, "*", Allow)`).
7. `Reject` returns a permission error, optionally carrying user feedback.

Pending asks coalesce using the same remember scope: native asks group only an
identical subject, while legacy asks retain action-wide grouping. The CLI TUI
and server receive ask requests through their existing surfaces. Headless
`exec`, RPC, and goal flows answer residual asks with `Reject`.

### Plugin permission bridge

`PermissionBridge` implements `PermissionInterceptor` over connected plugins
([`permission_bridge.rs`](../../crates/hya-plugin/src/permission_bridge.rs)):

1. **Resolution**: `permission.ask` is polled across plugins in load order. The
   **first** plugin that returns `allow_once`, `allow_always`, or `reject` wins.
   If every plugin defers (or every plugin errors), the host falls through to
   its normal interactive user prompt (`None` from the interceptor).
2. **Remembered grants are not plugin-keyed**: an `allow_always` from the bridge
   is stored like any other decision — either a legacy `Rule(action, "*", Allow)`
   on the persistent rule list or an `ExactSubject` in `native_grants`
   ([`permission.rs`](../../crates/hya-tool/src/permission.rs)). Those stores
   are **not** keyed by plugin identity and are **not** cleared when the plugin
   set changes.
3. **Semantic identity (fingerprint, not a decision cache)**: the bridge's
   `PermissionInterceptor::semantic_identity_v1` is a domain-separated SHA-256
   over `b"hya.plugin.permission-bridge.semantic-identity/v1"` plus, per plugin
   that registers `permission.ask`, its id, canonical initialize declaration,
   and effective posture. That digest is mixed into
   `PermissionPlane::semantic_identity_v1` and then
   `TurnBinding::semantic_fingerprint_v1` so runtime refresh can detect that
   permission **policy** changed — it does not index or invalidate remembered
   grants. See also
   [runtime.md — Permission policy semantic identity](runtime.md#permission-policy-semantic-identity).
4. **Wire resource**: `permission.ask` carries a tagged `WireResource` union with
   the nine variants listed above.

## External directory boundary

Paths use lexical workdir resolution: the workdir is absolutized and normalized
by removing `.` and textually popping `..`. Symlinks are **not** canonicalized,
so a symlink inside the workdir is not classified as external based on its
target.

For tools that **do** enforce the boundary, a resolved path **outside** the
session workdir triggers an `Action::ExternalDirectory` assert on the
containing directory's `<dir>/*` pattern **before** the normal Read/Edit/Lsp/…
check. Call-level invocation grants never satisfy `ExternalDirectory`, so it
prompts separately even inside an already-approved tool call.

### Enforcement points

| Tool | What is gated |
| --- | --- |
| `read` | Resolved file or directory path. |
| `write` | Resolved file path (`<parent>/*` when outside). |
| `edit` | Resolved file path (`<parent>/*` when outside). |
| `apply_patch` | Paths must be relative and must not escape the workdir (absolute or `..` components are **input errors**); each surviving path is then checked as `Action::Edit`. ExternalDirectory is not raised because escape is rejected first. |
| `lsp` | Resolved file path (`<parent>/*` when outside). |
| `glob` | Search root directory when outside the workdir. |
| `grep` | Search root (file or directory) when outside the workdir. |
| `shell` / `bash` | Optional `workdir` argument when it resolves outside the session workdir (`cwd/*`). |
| `find` | **Does not** perform the external-directory check (deliberate compatibility gap). Asserts `Action::Glob` only; builds the root with `PathBuf::from(path)` (not workdir-resolved). |
| `ls` | **Does not** perform the external-directory check either. Asserts only `Action::Read` on the raw path string; builds the directory with `PathBuf::from(path)` (not workdir-resolved). So `ls /etc` outside the workdir never raises `ExternalDirectory`. |

### Per-turn external directories

`SessionEngine::run_turn_with_external_dirs` (and the guidance/claim variants)
layers temporary allow rules onto the session permission snapshot for that turn
only
([`turn.rs`](../../crates/hya-core/src/engine/turn.rs)):

```text
Rule { action: ExternalDirectory, resource: "<dir>/*", mode: Allow }
```

for each directory in `external_dirs`. Directories the caller explicitly
attached therefore never prompt for that turn. The overlay is **not** persisted
as a `SessionPermissionSet` and does not survive the turn. The HTTP/Compat
prompt path derives the list from the session's reference directories
([`session_prompt.rs`](../../crates/hya-server/src/compat/session_prompt.rs)).

## Tool errors

Failed tools become `Event::ToolError` with a structured value:

```json
{ "error": { "type": "<kind>", "message": "<text>" } }
```

Mapping from `ToolError` to the wire `type` string
([`tool_error.rs`](../../crates/hya-core/src/engine/tool_error.rs)):

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

A thirteenth wire `type` is **not** a `ToolError` variant: when a
`tool.execute.before` hook vetoes a call, the engine emits
`Event::ToolError` with `value` built as
`tool_error_message_value("blocked", …)` and message text
`blocked by plugin: <reason>`
([`turn.rs`](../../crates/hya-core/src/engine/turn.rs)). Clients that switch on
this string should treat the twelve `ToolError` mappings **and** `blocked` as
first-class. `permission` errors are protected from rewriting by
`tool.execute.after` hooks; other outcomes may be rewritten by those hooks
([`turn.rs`](../../crates/hya-core/src/engine/turn.rs)).

## CLI Defaults

Under the default invocation model, local read-only tools and `task` allow;
standard built-ins, plugins, network reads, MCP calls, and shell commands ask.
The existing resource rules still auto-allow `Read`, `Glob`, and `Grep`, while
mutating, external-directory, subagent, and process-spawning actions remain
covered by their existing checks. `--yolo` changes the invocation model to
`danger` before the engine is built.

## Engine Integration

Provider decoders only request tool calls. `SessionEngine` runs before-hooks,
looks up the registered tool, validates its invocation metadata, authorizes it,
builds a `ToolCtx` with the call-scoped permission plane, executes it, runs
after-hooks, applies `cap_tool_output` on success, and appends either:

- `Event::ToolResult`
- `Event::ToolError`

The next provider round then sees the tool result in the projected transcript.
Unknown tools and malformed shell input fail before permission asks. Native asks
carry session, message, and tool-call correlation.

## External Tool Sources

`hya-app` prepares configured MCP and startup plugin sources, then submits their
complete declarations to one `RuntimeReconciler`. Only `RuntimeRegistry`
publishes the effective immutable view. MCP tools keep the external name
`mcp__<server>__<tool>`; plugin tool names remain as declared. Both sources use
the existing permission plane, tool result events, and projection replay as
builtin tools. Source metadata never becomes a second dispatch registry.

## Bundle sidecars (0.34.11)

Executable public Bundles compile one immutable `CompiledResourceView` from the
captured `TurnBinding`; it supplies both schema and dispatch. Only selected
canonical Tool IDs and hook IDs activate, and an alias never renames a hook.
Bundle-local tools resolve through the canonical namespace and Bundle-local
precedence, while host tools, static skills, and host-managed MCP remain
available according to the Harness view. The existing `PermissionPlane` and
plugin policy run before `tool/call`; denial produces no RPC, while an allowed
call uses the existing `ToolResult` path. Selected hook request/reply calls and
one-way event notifications remain activation-bound to the same captured
binding. Generic superset declarations reject. Bundle-declared MCP remains
unsupported, and a Bundle adds no permission plane or permission expansion.
