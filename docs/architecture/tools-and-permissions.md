# Tools and Permissions

The tool system lives in [`../../crates/hya-tool`](../../crates/hya-tool). The
engine exposes tool schemas to the model, then executes requested calls only
after permission checks pass.

## Tool Registry

[`tool.rs`](../../crates/hya-tool/src/tool.rs) defines:

- `Tool`: name, schema, async execute.
- `ToolCtx`: permission plane, interaction/spawner/todo/skill/websearch/LSP/
  formatter/`WorkflowPlane` planes, session ids, workdir, cancellation token.
- `ToolRegistry`: name-to-tool map, aliases, and model-facing schemas.

`ToolRegistry::builtins()` installs **27** canonical schema names before model
filtering ([`tool.rs`](../../crates/hya-tool/src/tool.rs)). The table below is
the complete inventory. Advertised fields are the model-facing JSON schema
`required`/`properties` keys; a schema marked **closed** rejects unknown keys.

| Tool | Input (advertised) | Output |
| --- | --- | --- |
| `invalid` | unknown call payload | Structured invalid-tool response. |
| `read` | `{ "path": string, "offset"?: integer >= 1, "limit"?: integer >= 1, "raw"?: boolean }` (closed) | Hashline or raw file text, media attachment, or directory listing. |
| `write` | `{ "path": string, "content": string }` (closed) | Final write result plus bounded display metadata and formatter/LSP diagnostics when available. |
| `edit` | `{ "path": string, "edits": [...] }` (closed; operations are `replace`, `append`, `prepend`, or `replace_text`) | Fresh hashline preview, bounded diff, warnings, and diagnostics. |
| `apply_patch` (`patch`) | `{ "patchText": string }` (alias `patch`) | Aggregate diff and per-file metadata. |
| `ls` | `{ "path"?: string }` | Immediate directory entries. |
| `glob` | `{ "pattern": string, "path"?: string }` | Path matches and counts (cap 100). |
| `find` | `{ "pattern": string, "path"?: string }` | `{path, size}` matches (no row cap). |
| `grep` | `{ "pattern": string, "path"?: string, "glob"?: string, "ignoreCase"?: boolean, "literal"?: boolean, "context"?: integer 0..5, "limit"?: integer 1..200 }` (closed) | Hashline match/context rows, summary, and bounded per-file display metadata. |
| `bash` | `{ "command": string, "env"?: object<string,string>, "timeout"?: number, "cwd"?: string, "pty"?: boolean }` (closed) | Structured command/output result with exit, timeout, truncation, and artifact metadata. |
| `webfetch` (`fetch`) | `{ "url": string, "format"?: "text"\|"markdown"\|"html", "timeout"?: number }` | Fetched web content or image attachment. |
| `websearch` (`search`) | `{ "query": string, "numResults"?: number, "livecrawl"?: string, "type"?: string, "contextMaxCharacters"?: number }` | Search results from the configured `WebSearchPlane`. |
| `question` | `{ "questions": [{ "question", "header", "options", "multiple"?, "custom"? }] }` | Chosen option labels (unanswered → `Unanswered`). |
| `ask_user` | `{ "question": string, "kind"?: "text"\|"select", "options"?, "allow_custom"?, "default"? }` | Answer object; cancellation returns `{ "answer": "", "cancelled": true }`. |
| `lsp` | `{ "operation", "filePath", "line", "character", "query"? }` | LSP provider response. |
| `skill` | `{ "name": string }` (name only; a path is not accepted) | `<skill_content>` envelope with body, `file://` base dir, and sampled files (cap 10). See also [`docs/skills.md`](../skills.md). |
| `list_agents` | (none) | Agent definitions usable by `task`. |
| `task` | `{ "description", "prompt", "subagent_type"?, "category"?, "model"?, "task_id"?, "command"?, "background"?, "resident"?, "inline_agent"?, "members"?: [...] }` | Foreground/background subagent outcomes. |
| `workflow` | `{ "action"?: "list"\|"info"\|"select"\|"run"\|"state", "name"?: string, "expected_revision"?: string, "inputs"?: object, "run"?: string }` | Shared app-owned Workflow control. |
| `announce` | `{ "body": string }` | One-way announcement to the caller's direct reports; recipients reply with ordinary mail. |
| `todowrite` (`todo`) | `{ "todos": [{ "content", "status", "priority" }] }` | Latest todo snapshot for the session (replace, not append). |
| `plan_exit` (`plan`) | plan status input | Plan-mode completion signal. |
| `send` | `{ "to": string, "body": string, "kind"?: "message"\|"announcement" }` | Mail delivery receipt. |
| `roster` | (none) | Live teammates with handle, type, status, task. |
| `channels` | (none) | Team channels with members and message counts. |
| `join` | `{ "channel": string }` | Subscribe (creates channel if missing). |
| `leave` | `{ "channel": string }` | Unsubscribe from a channel. |

`lsp` is a separate language-server contract and intentionally retains its
`filePath` field. This does not advertise or restore a legacy Read, Write, or
Edit schema; the coding-tool compatibility boundary is documented below.

`read.filePath` is a hidden, runtime-only compatibility spelling for captured
pre-0.36.9 calls. It is parsed separately from canonical `path`: one non-empty
value is accepted, equal non-empty values are accepted, conflicting non-empty
values fail as an input error, and both missing/empty values fail. Paths are not
trimmed. A legacy `offset: 0` is normalized to line 1, but zero is never
advertised. `shell` is the hidden runtime alias for canonical `bash`; it is not
advertised and uses the same command implementation. The nested
`task.inline_agent.description` field is absent from both published schemas:
an empty/whitespace-only stale value is normalized to absence, while a
non-empty direct or stale value retains the typed
`unsupported_inline_agent_field` rejection before admission. Other existing
aliases (`fetch`, `search`, `todo`, `patch`, and `plan`) remain non-advertised
lookup conveniences and do not change the coding-tool schemas.

### Coding-tool examples

Model-facing calls use the canonical fields below:

```json
{"path":"src/main.rs","offset":1,"limit":120,"raw":false}
```

```json
{"path":"src/main.rs","edits":[{"op":"replace","pos":"12#KT","lines":["new line"]}]}
```

```json
{"path":"notes.txt","content":"hello\n"}
```

```json
{"pattern":"TODO","path":"src","glob":"*.rs","ignoreCase":false,"literal":true,"context":1,"limit":20}
```

```json
{"command":"cargo check","cwd":".","timeout":300,"env":{"RUST_BACKTRACE":"1"},"pty":false}
```

The Bash example demonstrates the input shape only; environment values are
never shown in titles, summaries, diagnostics, or rendered output.

## Output Limits

Two stacked caps apply. Per-tool caps run inside the tool; a global shape-aware
cap runs after every successful result and again after post-tool hooks at the
last point before `Event::ToolResult` publication. Coding-tool adapters keep the
result envelope structured so presentation metadata is not discarded by a
generic string cap.

### Per-tool caps

- **`read` / `grep`**: model-facing text and match output is bounded at **50 KiB**;
  line/row limits and explicit truncation/continuation metadata remain in the
  result. Grep caller globs stop at 4,096 bytes, retained logical lines stop at
  1 MiB, and ignore inputs/rule counts are bounded. An overlong line or ignore
  source produces one bounded warning and later searchable content remains
  available. Traversal, ignore parsing, line discard, and matching all observe
  cancellation.
- **`edit`**: fresh hashline output and the unified diff each have independent
  bounded budgets; metadata carries explicit truncation rather than growing
  without limit. Hashline error messages stop at 8 KiB, keep at most 16 hints,
  and bound each hint to 512 bytes while preserving the stable `[E_*]` code.
- **`bash`**: stdout and stderr are consumed in arrival order into a bounded
  sink with **50 KiB** inline output. Timeout/clamp notices are added before the
  inline/spill decision. When that result crosses the limit, capture keeps the
  complete raw output in a private mode-0600 hya artifact and retains only the
  bounded inline view. An armed owner removes every unpublished artifact on
  error or cancellation. `env` values are never copied into titles,
  diagnostics, results, or the TUI.
- **`glob`**: caller patterns stop at 4,096 bytes; returned rows remain capped
  at `SEARCH_LIMIT = 100`. `find` keeps its existing compatibility behavior.

### Global cap (`cap_tool_output`)

After a successful builtin, MCP, or plugin call, the engine passes the result
through `hya_tool::cap_tool_output_with_policy`. Post-tool hooks may replace the
result, so the direct execution path reapplies the same cap immediately before
emitting `Event::ToolResult`. The default for unrelated results remains
`MAX_TOOL_OUTPUT_CHARS = 5000` characters.
Coding-tool results use a structurally idempotent policy: each nested row/group
is serialized once for byte accounting, bounded Read/Write/Grep/Bash envelopes
and Edit diff metadata remain objects, and independent hard limits set explicit
truncation flags. No metadata or hook can bypass the final cap. Provider replay
uses an object's string `output` field when present and falls back to JSON only
when no such field exists.

## Coding-tool runtime

`ReadTool`, `EditTool`, `WriteTool`, and `GrepTool` share one private,
registry-owned `HashlineRuntime`; it is not a second result store or projection.
The runtime owns only bounded process-local snapshots, duplicate/no-op guards,
and fixed mutation-lock shards. Its state key is
`(SessionId | no-session, normalized workdir, resolved target path)`, so one
session cannot recover content observed by another. It retains at most eight
target entries, four newest versions per target, and 32 MiB total snapshot
bytes. A fixed lock-shard array serializes same-target mutations without an
attacker-sized lock map; Unix hard-linked aliases share device/inode identity.
Prepared device/inode identity is checked again immediately before ordinary
rename or hard-link truncate/open, so a pathname/alias swap fails before
mutation.

Text Read/Grep normalization removes one UTF-8 BOM and normalizes CRLF/lone CR
to LF. Both accept `[!x]` and `[^x]` negated character classes consistently in
caller and ignore patterns. Each visible line gets a contextual XXH32 seed-0
anchor from its previous/current/next lines, with a two-character nibble hash
by default; the terminal empty newline sentinel is not rendered. `raw` Read
returns normalized text without anchors. Anchors are stale-reference aids,
never integrity or authorization data.

Edit validates every `LINE#HASH` anchor against one pre-edit snapshot, checks
text hints and collisions, resolves all spans before applying them bottom-up,
and rejects duplicates, conflicts, invalid payloads, and edits that would make a
non-empty file empty. On an `E_STALE_ANCHOR` failure only, it tries newest-first
stored snapshots and an exact context-three, fuzz-zero merge; it never fuzzy
relocates an anchor. Fresh anchors, diffs, display metadata, and stored
snapshots describe the final post-formatter bytes. The target lock covers live
read, validation/recovery, mutation, formatter, LSP, final read, diff, and state
update. If cancellation arrives after commit, reconciliation records the actual
bytes and duplicate guard before returning typed `cancelled`. Two identical
no-op payloads are soft successes; the third returns `[E_NOOP_LOOP]`; a non-raw
Read clears the no-op/duplicate marker.

All coding tools honor `ToolCtx` cancellation. Read and Grep authorize one
kind-blind lexical external parent resource before metadata, existence, or
target-kind probing. Cancellation while waiting for a lock or during I/O
returns the typed `cancelled` error. Bash cancellation/timeout terminates and
reaps the complete process group, including a PTY descendant that retains the
slave after leader exit. File contents never enter logs or error payloads.
Durable `Event::ToolResult`/`Event::ToolError` and the shared projection remain
the only result path. Hashline snapshots are process-local and are lost on
restart; current-file anchor validation still works after restart.

An already-running **0.36.8** backend must restart before future calls use these
0.36.9 contracts. Existing 0.36.8 error Events are historical append-only data;
they are not rewritten or retried.

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
| `bash` | `Bash` | `bash`, hidden runtime alias `shell`, and invocation command subjects. |
| `task` | `Task` | `task` (per member / subagent type). |
| `mcp` | `Mcp` | MCP bridge tools (`mcp__…`). |
| `webfetch` | `WebFetch` | `webfetch`. |
| `websearch` | `WebSearch` | `websearch`. |
| `todowrite` | `TodoWrite` | `todowrite`. |
| `skill` | `Skill` | `skill`. |
| `lsp` | `Lsp` | `lsp`. |
| `externaldirectory` | `ExternalDirectory` | Any tool whose resolved path (or Bash `cwd`) lies outside the session workdir. |

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
tools expose only their namespaced `mcp` name, and Bash exposes its canonical
tool name and the full command after before-hooks. Registry metadata, not a
name-prefix check, determines which domain applies.

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
| `bash` (including hidden `shell`) | Optional `cwd` when it resolves outside the session workdir (`<cwd>/*`). |
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
standard built-ins, plugins, network reads, MCP calls, and Bash commands ask.
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
Unknown tools and malformed Bash input fail before permission asks. Native asks
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
