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

The builtin registry includes:

| Tool | Input | Output |
| --- | --- | --- |
| `invalid` | unknown call payload | Structured invalid-tool response. |
| `read` | `{ "path": string }` | File text/media or directory listing. |
| `write` | `{ "path": string, "content": string }` | Write result plus formatter/LSP diagnostics when available. |
| `edit` | `{ "path": string, "old": string, "new": string }` | Replacement result plus diff/formatter/LSP data. |
| `apply_patch` (`patch`) | patch text | Aggregate diff and per-file metadata. |
| `ls` | `{ "path"?: string }` | Immediate directory entries. |
| `glob`, `find` | pattern/path inputs | Path matches and counts. |
| `grep` | `{ "pattern": string, "path"?: string, "include"?: string }` | Regex matches and counts. |
| `shell`, `bash` | `{ "command": string }` | Command title, stdout/stderr, exit status. |
| `webfetch` (`fetch`) | URL input | Fetched web content via the web-fetch tool. |
| `websearch` (`search`) | query input | Search results from the configured `WebSearchPlane`. |
| `question`, `ask_user` | prompt/options input | Human answer or cancellation. |
| `lsp` | operation input | LSP provider response. |
| `skill` | skill path/name input | Skill content. |
| `task` | member prompts | Foreground/background subagent outcomes. |
| `todowrite` (`todo`) | todo items | Latest todo snapshot for the session. |
| `plan_exit` (`plan`) | plan status input | Plan-mode completion signal. |

## Output Limits

Large string outputs are truncated at 16 KiB and include a truncation marker.
Search-style tools cap returned entries while preserving counts and truncation
metadata. These limits keep provider context from being flooded by accidental
large outputs.

## Permission Models

[`permission.rs`](../../crates/hya-tool/src/permission.rs) defines:

| Type | Meaning |
| --- | --- |
| `InvocationPolicy` | Compiled ordered regex rules and the active invocation model. |
| `Invocation` | Canonical tool, MCP, and post-hook command subjects for one call. |
| `Action` | Resource operation category such as `Read`, `Edit`, `Grep`, or `Bash`. |
| `Resource` | Path, glob, command, subagent, or any resource. |
| `Mode` | `Allow`, `Ask`, or `Deny`. |
| `Rule` | Action + resource pattern + mode. |
| `Decision` | User response: allow once, allow always, or reject with optional feedback. |
| `PermissionPlane` | Invocation policy, resource rules, remembered grants, and ask channel. |

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
   remembered grant.
2. If not, it sends an `AskRequest` containing action, resource, and a reply
   channel.
3. The caller answers with a `Decision`.
4. `AllowOnce` permits only the current call.
5. Native invocation `AllowAlways` remembers only the selected exact target and
   value. Legacy resource `AllowAlways` continues to allow the whole action.
6. `Reject` returns a permission error, optionally carrying user feedback.

Pending asks coalesce using the same remember scope: native asks group only an
identical subject, while legacy asks retain action-wide grouping. The CLI TUI
and server receive ask requests through their existing surfaces. Headless
`exec`, RPC, and goal flows answer residual asks with `Reject`.

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
after-hooks, and appends either:

- `Event::ToolResult`
- `Event::ToolError`

The next provider round then sees the tool result in the projected transcript.
Unknown tools and malformed shell input fail before permission asks. Native asks
carry session, message, and tool-call correlation.

## External Tool Sources

`hya-backend` registers MCP tools from `hya-mcp` after connecting configured
servers. Those tools are named `mcp__<server>__<tool>`. It then registers plugin
tools from `hya-plugin`. Both sources use the same registry, permission plane,
tool result events, and projection replay as builtin tools.
