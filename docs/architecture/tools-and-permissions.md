# Tools and Permissions

The tool system lives in [`../../crates/yaca-tool`](../../crates/yaca-tool). The
engine exposes tool schemas to the model, then executes requested calls only
after permission checks pass.

## Tool Registry

[`tool.rs`](../../crates/yaca-tool/src/tool.rs) defines:

- `Tool`: name, schema, async execute.
- `ToolCtx`: permission plane, workdir, cancellation token.
- `ToolRegistry`: name-to-tool map plus model-facing schemas.

The builtin registry includes:

| Tool | Input | Output |
| --- | --- | --- |
| `read` | `{ "path": string }` | `{ "content": string }` |
| `write` | `{ "path": string, "content": string }` | `{ "ok": true, "bytes": number }` |
| `edit` | `{ "path": string, "old": string, "new": string }` | `{ "replaced": true }` |
| `glob` | `{ "pattern": string }` | `{ "paths": string[], "total": number }` |
| `grep` | `{ "pattern": string, "path"?: string }` | `{ "matches": object[], "total": number }` |
| `shell` | `{ "command": string }` | `{ "stdout": string, "stderr": string, "exit_code": number? }` |

## Output Limits

Large string outputs are truncated at 16 KiB and include a truncation marker.
`glob` and `grep` return at most 500 items while preserving the total match
count. These limits keep provider context from being flooded by accidental large
outputs.

## Permission Model

[`permission.rs`](../../crates/yaca-tool/src/permission.rs) defines:

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

Mutating or process-spawning actions such as `Edit` and `Bash` default to `Ask`.

## Engine Integration

Provider decoders only request tool calls. `SessionEngine` looks up the named
tool, builds a `ToolCtx`, executes it, measures elapsed time, and appends either:

- `Event::ToolResult`
- `Event::ToolError`

The next provider round then sees the tool result in the projected transcript.
