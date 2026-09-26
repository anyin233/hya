# Tools and Permissions

The tool system lives in [`../../crates/hya-tool`](../../crates/hya-tool). The
engine exposes tool schemas to the model, then executes requested calls only
after permission checks pass.

The interface crate owns the registry and session-scoped service planes.
All 27 concrete built-in tool bodies and schemas live in five family sources
under [`../../bundles/presets`](../../bundles/presets). They are packaged as
lockstep Rust libraries and loaded by `ToolRegistry::builtins()`. The tools
use session-scoped planes through `ToolCtx`; plane methods and request types
remain in `hya-tool` so host authority stays with the session runtime.

## Tool Registry

[`tool.rs`](../../crates/hya-tool/src/tool.rs) defines:

- `Tool`: name, schema, async execute.
- `ToolCtx`: permission plane, interaction/spawner/todo/skill/websearch/LSP/
  formatter/`WorkflowPlane` planes, session ids, workdir, workspace `roots`,
  cancellation token. `roots` are the session's ordered, deduplicated
  workspace roots resolved at turn start (ADR-0024; see
  [Workspace roots](runtime.md#workspace-roots)). The file tools judge
  their path boundary against these roots through `ProjectScope`
  ([External directory boundary](#external-directory-boundary)).
- `ToolRegistry`: name-to-tool map, aliases, and model-facing schemas.

`ToolRegistry::builtins()` installs **27** canonical schema names before model
filtering ([`tool.rs`](../../crates/hya-tool/src/tool.rs)). The table below is
the complete inventory. Advertised fields are the model-facing JSON schema
`required`/`properties` keys; a schema marked **closed** rejects unknown keys.

### Tool namespaces

Provider tool-name charsets (OpenAI/Anthropic function names) allow only
`[a-zA-Z0-9_-]`, so `:`/`/` can never reach a provider. Namespaces therefore
use a **double-underscore separator**: `namespace__local` — the same
convention MCP tools already use model-facing (`mcp__server__tool`). A
namespace groups the tools of one functionality provider; local names may
repeat across namespaces (`todo__read` vs `pluginx__read`) while full names
stay globally unique inside a registry. `hya-tool` exposes the mechanism in
[`namespace.rs`](../../crates/hya-tool/src/namespace.rs):

- `namespaced_name(namespace, local)` composes the canonical name; both
  tokens must be non-empty, use only `[a-zA-Z0-9_-]`, and contain no `__`.
  Invalid tokens return `InvalidNamespacedName`.
- `namespace_of(name)` returns the namespace segment under a first-segment
  rule (`mcp__server__tool` → `mcp`); plain names and degenerate spellings
  (`__x`, `x__`) return `None`.
- `ToolRegistry::register_namespaced(namespace, local, tool)` (or
  `..._with_permission`) validates the tokens, requires the tool's own
  `name()` to equal the composed canonical name (so the advertised schema
  and the registry key cannot drift apart), and rejects duplicate full
  names via `NamespacedRegisterError`.

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
| `ask_user` (`question`) | `{ "questions": [{ "question", "header", "options": [{ "label", "description" }], "multiple"?, "allow_custom"?, "default"? }] }` (alias `question`) | Structured per-question answers `{question, answer, cancelled}`; unanswered renders as `Unanswered`; plane failures error. |
| `lsp` | `{ "operation", "filePath", "line", "character", "query"? }` | LSP provider response. |
| `skill` | `{ "name": string }` (name only; a path is not accepted) | `<skill_content>` envelope with body, `file://` base dir, and sampled files (cap 10). See also [`docs/skills.md`](../skills.md). |
| `list_agents` | (none) | Agent definitions usable by `task`. |
| `task` | `{ "description", "prompt", "subagent_type"?, "category"?, "model"?, "command"?, "inline_agent"?, "members"?: [...] }` | Non-blocking: per member `{member (handle), session, status: running}`; results arrive as the member's report mail. Every member is a resident actor. |
| `workflow` | `{ "action"?: "list"\|"info"\|"select"\|"run"\|"state", "name"?: string, "expected_revision"?: string, "inputs"?: object, "run"?: string }` | Shared app-owned Workflow control. |
| `todo__read` | `{}` | Current items with stable ids and statuses. |
| `todo__update_status` | `{ "updates": [{ "id", "status": "pending"\|"in_progress"\|"blocked"\|"completed" }] }` | Full snapshot after batch status updates. |
| `todo__update_content` | `{ "operations": [{ "op": "add", "content" } \| { "op": "remove", "id" } \| { "op": "edit", "id", "content" }] }` | Full snapshot after batch content edits (atomic; adds report assigned ids). |
| `plan_exit` (`plan`) | plan status input | Plan-mode completion signal. |
| `send` | `{ "channel"?: string, "body": string }` — `#channel`/channel id/handle/`^parent`; omitted = role default | Delivery receipt: group channel = broadcast announcement, DM channel/handle = private mail (archived child revives), default = led unit or parent. `^parent` auto-infers the DM channel minted with the direct parent at registration time (works even when the parent handle is not yet in the roster, and at any depth — it never targets the root); without a DM channel it falls back to the parent handle. |
| `list_channel` | (none) | The caller's channels: group pipes with can-post flag, DM channels with peer + unread. |
| `search_agent` | `{ "query"?: string }` | The caller's archived direct children (handle, digests, degraded flag). |
| `wait` | `{ "targets"?: string[], "mode"?: "all"\|"any", "timeout_secs"?: integer 0..1800 }` — omitted targets = every live direct subagent | `{woke_by: members\|mail\|timeout\|nothing_to_wait_for, finished[], running[], mail[], waited_ms}`; blocks the turn until the condition, bounded; the channel-tools override also wakes on mail. Permission class `read_only`. |
| `archive` | `{ "target": string, "reason"?: string }` — handle, leaf, or session id of a live descendant | `{handle, session, cancelled_turn, descendants}`: the in-flight turn is cancelled (`cause: archived`) and the member archived; mail to its handle wakes it. Permission class `task`. |

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
  diagnostics, results, or any client surface.
- **`glob`**: caller patterns stop at 4,096 bytes; returned rows remain capped
  at `SEARCH_LIMIT = 100`. `find` keeps its existing compatibility behavior.

### Global cap (`cap_tool_output`)

After a successful builtin, MCP, or plugin call, the engine passes the result
through `hya_tool::cap_tool_output_with_policy`. Post-tool hooks may replace the
result, so the direct execution path reapplies the same cap immediately before
emitting `Event::ToolResult`. The default for unrelated results remains
`MAX_TOOL_OUTPUT_CHARS = 5000` characters. An ordinary result over that limit
becomes a string that keeps both ends: a notice line, the first
`TRUNCATED_HEAD_CHARS = 2000` characters, a marker naming the omitted size
and the artifact holding the complete result, then the last
`TRUNCATED_TAIL_CHARS = 2500` characters:

```text
[tool output truncated: original 9120 chars; showing first 2000 and last 2500 chars. Full output: artifact://01J… — read that handle for the rest]
<first 2000 chars>
[… 4620 chars omitted; full output: artifact://01J… …]
<last 2500 chars>
```

Before 0.41.0 only the last 5000 characters were kept, which dropped
headers and leading JSON keys. An object result is measured and cut as its
serialized JSON, `metadata` included. A tool whose structure must survive
bounds itself below the cap, as `wait` does. Coding-policy results
(`read`, `grep`, `bash`, …) are not affected: they keep their own
structured caps.
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
kind-blind external parent resource (the target's lexical parent) before
metadata, existence, or target-kind probing. Cancellation while waiting for a lock or during I/O
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
| `SessionPermissionMode` (hya-core) | Per-session-tree mode (`manual`, `yolo`, bundle mode) from which each call's plane is derived. |

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
| `externaldirectory` | `ExternalDirectory` | A file tool whose resolved path lies outside every workspace root of the session, or a Bash `cwd` outside the session workdir. |

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
   swapping interceptors changes the policy fingerprint. The process-wide
   interceptor is the plugin `PermissionBridge`; per call the engine prepends
   the bundle activation's `permission.ask` hooks and, under a bundle
   permission mode, appends that bundle's `permission.approve` approver (see
   [Session permission modes](#session-permission-modes)). A direct shell turn
   additionally prepends `UserShellApproval` ahead of all of them (see
   [Direct shell turns](#direct-shell-turns)).
3. If still unresolved, it sends an `AskRequest` containing action, resource,
   and a reply channel.
4. The caller answers with a `Decision`.
5. `AllowOnce` permits only the current call.
6. Native invocation `AllowAlways` remembers only the selected exact target and
   value. Legacy resource `AllowAlways` continues to allow the whole action
   (`Rule(action, "*", Allow)`).
7. `Reject` returns a permission error, optionally carrying user feedback.

### Saved grants

An "allow always" answered by a client through the server
(`POST /v1/interactions/{id}/respond`) is also persisted as a
`saved_permission` row (`id`, `project_id` — always `global`, `action` — the
serde name above, `resource` — the exact subject value or `*`,
`time_created` — ms since the epoch; migration `0013` added it, older rows
keep it `NULL`). Grants are process-wide because the permission plane is:
every session plane is derived from the one process plane and shares its
remembered grants.

- **Startup.** `AppState::restore_saved_permissions` (called by `hya serve`
  and the in-process runtime before serving) replays every row into the
  process plane with `PermissionPlane::grant_saved`: `*` restores the
  action-wide `Rule(action, "*", Allow)`; any other value restores the exact
  subject for `tool` (`PermissionTarget::Tool`), `mcp` (`Mcp`), and `bash`
  (`Command`).
- **Delete.** `DELETE /v1/permissions/rules/{rule}` removes the row and calls
  `PermissionPlane::revoke_saved`, so the next matching call asks again.
  Configured (snapshot) rules are never touched.
- **Listing.** `GET /v1/permissions/rules` reports each row as a
  `SavedRule` with `permission: RULE_PERMISSION_ALLOW` and its
  `timeCreated`; see the [protocol guide](../protocol/README.md#saved-permission-rules).

Grants answered by an interceptor (plugin bridge, bundle approver) stay
in-memory only.

Pending asks coalesce using the same remember scope: native asks group only an
identical subject, while legacy asks retain action-wide grouping. The server
surfaces pending asks to connected clients through its interaction endpoints.
Headless `exec`, RPC, and goal flows answer residual asks with `Reject`.

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

The boundary is the session's workspace roots (`ToolCtx::roots`; ADR-0024,
ADR-0026), not the workdir alone. One helper,
[`ProjectScope`](../../crates/hya-tool/src/project_scope.rs), decides
containment for every file tool:

- **Roots** are canonicalized once per call (symlinks resolved). A root that
  cannot be resolved keeps its lexical absolute form. A hand-built context
  with no roots falls back to `[workdir]`.
- **Candidate paths** resolve against the workdir when relative, then are
  canonicalized. A path that does not exist yet (a `write` target) is judged
  by canonicalizing its nearest existing ancestor and re-appending the
  missing remainder; a `..` in that remainder makes it outside. A dangling
  symlink is judged by where it would land.
- **Containment** is component-wise (`/a/b` does not contain `/a/bc`) and
  holds if **any** root contains the path, so nested and overlapping roots
  are fine.
- **Outside** means: outside every root after resolution. A symlink inside a
  root that points elsewhere is outside; a symlink elsewhere that points into
  a root is inside. A path whose resolution fails for any other reason
  (permission denied, symlink loop) is treated as outside, so it asks.

For a path outside, the tool asserts `Action::ExternalDirectory` on a
concrete `<dir>/*` pattern **before** the normal Read/Edit/Lsp/… check. The
pattern is built from the lexical path the tool was given (not the resolved
target). The assert goes through the normal `PermissionPlane` order, so
`danger` (yolo) and `allow` models auto-approve it, and a bundle mode's
`permission.approve` interceptor receives it like any other ask. Call-level
invocation grants never satisfy `ExternalDirectory`, so it prompts separately
even inside an already-approved tool call. Canonicalization costs a few
`stat`/`realpath` calls per tool call.

### Enforcement points

| Tool | What is gated |
| --- | --- |
| `read` | Resolved file or directory path (`<parent>/*` when outside). |
| `write` | Resolved file path (`<parent>/*` when outside). |
| `edit` | Resolved file path (`<parent>/*` when outside). |
| `apply_patch` | Relative paths resolve against the workdir; absolute paths are accepted. A `..` component is an **input error**, and so is a path outside every root (including through a symlink). ExternalDirectory is never raised; each surviving path is then checked as `Action::Edit`. |
| `lsp` | Resolved file path (`<parent>/*` when outside). |
| `glob` | Search root when outside (kind-blind `<parent>/*`). |
| `grep` | Search root, file or directory, when outside (kind-blind `<parent>/*`). |
| `find` | Resolved search root directory when outside (`<root>/*`); asserts `Action::Glob` on the pattern first. |
| `ls` | Resolved directory when outside (`<dir>/*`), then `Action::Read` on the directory. |
| `bash` (including hidden `shell`) | Optional `cwd` when it resolves outside the session workdir (`<cwd>/*`, lexical). |

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
as a `SessionPermissionSet` and does not survive the turn. The v1 turn path
derives the list from the session's reference directories
([`reference.rs`](../../crates/hya-server/src/support/reference.rs)).

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
| `WorkflowControl { code, message }` | the control `code` itself (e.g. `WORKFLOW_BUSY`) |
| `UnknownAgentId` | `unknown_agent_id` |
| `AgentSpawnNotAllowed` | `agent_spawn_not_allowed` |
| `UnsupportedInlineAgentField` | `unsupported_inline_agent_field` |
| `Other` | `unknown` |

A fourteenth wire `type` is **not** a `ToolError` variant: when a
`tool.execute.before` hook vetoes a call, the engine emits
`Event::ToolError` with `value` built as
`tool_error_message_value("blocked", …)` and message text
`blocked by plugin: <reason>`
([`turn.rs`](../../crates/hya-core/src/engine/turn.rs)). Clients that switch on
this string should treat the thirteen `ToolError` mappings **and** `blocked` as
first-class. `permission` errors are protected from rewriting by
`tool.execute.after` hooks; other outcomes may be rewritten by those hooks
([`turn.rs`](../../crates/hya-core/src/engine/turn.rs)).

### Session permission modes

Each session tree has a permission mode (`manual`, `yolo`, or a
bundle-declared `<bundle-id>/<mode-id>`), recorded on the root session by
`session_permission_mode_set` and folded into `SessionProjection.permission_mode`
(user-facing contract: [Configuration — Session permission
modes](../configuration.md#session-permission-modes)). The engine never
mutates the process-wide `PermissionPlane`. Instead, for **every tool call**
(model tool calls in `engine/turn.rs` and direct shell turns in
`engine/shell.rs`, including subagent and resident turns) it reads the
root's mode and derives that call's plane (`permission_mode::derive_plane`):

| Mode | Derived plane |
| --- | --- |
| `yolo` | `PermissionPlane::with_invocation_model(Danger)`: allow immediately, including explicit Deny rules. |
| `manual` | The process plane; a process-level `Danger` (`--yolo`, `model: danger`) is lowered to `Default` so asks really reach the user. |
| bundle mode | `manual`, plus `PermissionPlane::append_interceptor(ModeApprover)`: after the activation `permission.ask` hooks and the plugin `PermissionBridge` defer, the declaring bundle's `permission.approve` hook answers; `None` (defer, failure) falls through to the user ask. A mode whose bundle no longer publishes it behaves as `manual`. |

A tree without a recorded mode uses `yolo` when the process invocation model
is `Danger`, else `manual`. The derived plane shares the process plane's
rules, remembered grants, and ask channel, so an Allow Always given under one
mode still applies after a switch.

**In-flight semantics.** Because the mode is read at each call's permission
check, a switch applies to the next check of every session in the tree,
including turns already running; a call that was already authorized keeps
running (its tool-internal resource checks use the plane it was authorized
with). Switching to `yolo` through the v1 API additionally resolves the
tree's pending asks as allow-once (see
[event-model.md — Pending permission plane](event-model.md#pending-permission-plane-server-side)).

**Fingerprints.** `TurnBinding::semantic_fingerprint_v1` (and so the Workflow
request hash) keeps using the process plane's `semantic_identity_v1`: the
mode is mutable session state, not part of a runtime or request identity, so
a mode switch does not make a retried Workflow run look like a different
request. A bundle's declared modes are part of its runtime source identity
only when it declares any, so bundles without modes keep their fingerprint.

### Direct shell turns

A direct shell turn (`SessionEngine::run_shell`, the v1 `ShellTurn` behind the
TUI's `!command`) runs a command the user typed. Its call plane is derived as
above and then gets `UserShellApproval`
([`engine/shell.rs`](../../crates/hya-core/src/engine/shell.rs)) prepended as
the outermost interceptor: it answers `AllowOnce` for `Bash` and `Tool`
checks, so the user is never asked about their own command in any mode, and
the activation `permission.ask` hooks, the plugin `PermissionBridge`, and a
bundle mode's `permission.approve` approver are not consulted. Because
interceptors run only where the plane would otherwise ask, an explicit Deny
(invocation or resource rule) still fails the call, and the
`tool.execute.before` veto runs before authorization. It defers on
`ExternalDirectory`, so a check for a directory outside the working
directory still asks. `AllowOnce` records no grant. Model-issued tool calls
(`engine/turn.rs`) never get this interceptor.

## CLI Defaults

Under the default invocation model, local read-only tools and `task` allow;
standard built-ins, plugins, network reads, MCP calls, and Bash commands ask
(the user's own direct shell commands excepted; see
[Direct shell turns](#direct-shell-turns)).
The existing resource rules still auto-allow `Read`, `Glob`, and `Grep`, while
mutating, external-directory, subagent, and process-spawning actions remain
covered by their existing checks. `--yolo` changes the invocation model to
`danger` before the engine is built, which makes `yolo` the default session
permission mode; a session switched to `manual` still asks.

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
