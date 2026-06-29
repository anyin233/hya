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

## Permission Model

[`permission.rs`](../../crates/hya-tool/src/permission.rs) defines:

| Type | Meaning |
| --- | --- |
| `Action` | Operation category such as `Read`, `Edit`, `Grep`, or `Bash`. |
| `Resource` | Path, glob, command, subagent, or any resource. |
| `Mode` | `Allow`, `Ask`, or `Deny`. |
| `Rule` | Action + resource pattern + mode. |
| `Decision` | User response: allow once, allow always, or reject with optional feedback. |
| `PermissionPlane` | Snapshot rules, persistent allow-always rules, and ask channel. |

Rule matching uses a small `*` wildcard matcher. When multiple rules match, the
last matching rule wins.

## Ask Flow

When an action evaluates to `Ask`:

1. `PermissionPlane` checks whether a previous allow-always decision permits the
   action.
2. If not, it sends an `AskRequest` containing action, resource, and a reply
   channel.
3. The caller answers with a `Decision`.
4. `AllowOnce` permits only the current call.
5. `AllowAlways` adds a persistent allow rule for the whole action.
6. `Reject` returns a permission error, optionally carrying user feedback.

The CLI TUI receives ask requests and renders a permission panel. Headless flows
that do not service the ask channel will fail asked permissions with a channel
error.

## CLI Defaults

The binary builds a `PermissionPlane` that auto-allows:

- `Read`
- `Glob`
- `Grep`

Mutating, external-directory, subagent, and process-spawning actions such as
`Edit` and `Bash` default to `Ask`. `--yolo` installs a headless/TUI policy that
auto-approves all actions.

## Engine Integration

Provider decoders only request tool calls. `SessionEngine` looks up the named
tool, builds a `ToolCtx`, executes it, measures elapsed time, and appends either:

- `Event::ToolResult`
- `Event::ToolError`

The next provider round then sees the tool result in the projected transcript.

## External Tool Sources

`hya-cli` registers MCP tools from `hya-mcp` after connecting configured
servers. Those tools are named `mcp__<server>__<tool>`. It then registers plugin
tools from `hya-plugin`. Both sources use the same registry, permission plane,
tool result events, and projection replay as builtin tools.
