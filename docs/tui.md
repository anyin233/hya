# OpenTUI frontend

The `packages/hya-tui` frontend is a basic terminal client for a running
`hya serve` process. It uses OpenTUI for display and input while the
backend remains the owner of sessions, event history, tool execution, and
permissions. The screen is one main column (the transcript of the open
session, pending interactions, the status line, and the input) plus a
sidebar with the session list, todos, and session context that you can show
or hide (see [Layout](#layout)). Assistant replies render as Markdown with
highlighted code blocks; reasoning is collapsed to one `Thinking` line (see
[Messages](#messages)). Models, Workflows, and saved provider keys have
dedicated views; the API command view exposes the other HTTP/JSON operations
in `hya.v1`. The input is a multi-line editor with input history; it also
runs `!command` shell turns, completes `@file` references, and opens a
command menu on `/` (see [Composer](#composer)). Tab completes slash commands
using the TUI and server command catalogs. One persistent instruction line
stays below the input at the bottom of the screen and changes with the
current view.
If a backend predates the saved-key list endpoint, the main TUI still opens and
shows that key listing needs a backend restart with an updated binary.

## Start it

Requires Bun 1.4.2 (the version the repository pins; the Solid setup is verified on it) and a terminal supported by OpenTUI. From a clone:

```sh
cd packages/hya-tui
bun install --frozen-lockfile
```

Run these in separate terminals from the repository root:

```sh
cargo run --locked -p hya-backend --bin hya -- serve --bind 127.0.0.1:8080 --db "$HOME/hya-sessions.db"
bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080 --dir "$PWD"
```

`--server` is the backend base HTTP URL (default `http://127.0.0.1:8080`).
`--dir` is the absolute directory scope sent as `x-hya-directory` (default:
the frontend process's working directory). `--help` prints the launch syntax.
The backend's offline echo model is sufficient for a first run; configure a
provider in the backend for live model calls.

Type a plain prompt and press Enter. The frontend creates a session when none
is open, admits the prompt as a turn, and streams the reply into the
transcript as it arrives (see [Streaming, queued prompts, and turn
status](#streaming-queued-prompts-and-turn-status)). For example, type `summarize this repository`,
then `/models` to inspect available routes, and `/open 1` to return to the
first session. Press Ctrl+C twice (or Ctrl+D on an empty input, or type
`/exit`) to exit and restore the terminal.

To set a provider API key, type `/key set anthropic`, paste the key into the
concealed prompt, and press Enter. The prompt draws bullets only and clears its
buffer after submission; Esc cancels. `/keys` lists saved provider IDs, and
`/key remove anthropic` deletes that provider's saved credential. The backend
stores the key in its user auth directory; it never sends existing key values
back to the TUI. Configure that provider's model route in the backend config,
then restart the backend after adding or removing a key so the route resolves
the new credentials. OAuth login remains available through the backend CLI.
After typing `/keys`, read the bottom row: it shows `/key set <provider>` to
add or replace a key and `/key remove <provider>` to delete one. During
concealed entry, the row changes to `Paste API key · Enter saves · Esc cancels`.

If `/keys` says key listing is unavailable, restart the backend with hya
0.41.0 or newer and run the same frontend command again. For example, a
frontend on `127.0.0.1:22103` can reconnect after restarting the backend on
that port; sessions and other main views remain available while its older
backend is running.

## Commands and keys

| Input | Effect |
| --- | --- |
| Plain text + Enter | Admit a prompt in the current session; create one if needed. |
| Ctrl+J, Alt+Enter, Shift+Enter | Insert a newline instead of sending (Shift+Enter only where the terminal reports it; see [Composer](#composer)). |
| Up / Down | On the input's first / last line: the previous / next submitted input. |
| `!<command>` + Enter | Run the command as a shell turn in the current session (see [Shell turns](#shell-turns)). |
| `@<text>` | Show matching file paths; Up/Down select, Tab or Enter inserts `@<path>`, Esc closes (see [File references](#file-references)). |
| `/` at the start of the input | Open the command menu; fuzzy-filters as you type the name (see [Command menu](#command-menu)). |
| Esc | Close the command menu or the file list; else cancel the running turn; else clear the input. |
| Ctrl+C | Clear the input and show `Press Ctrl+C again to quit`; a second Ctrl+C within 2 s quits. |
| Ctrl+D | Quit when the input is empty (otherwise delete the character under the cursor). |
| `/exit`, `/quit` | Quit. |
| `/new [agent] [model]` | Create a session in `--dir`, using the first visible agent and its model by default. |
| `/sessions`, `/open <id or number>` | Refresh or switch sessions. |
| `/models`, `/model [provider/model]` | View catalog or change the selected session model; with no argument, shows the current model and the available list. |
| `/agent [name]` | Change the selected session's agent, or with no argument show the current agent and the available list. |
| `/rename <title>` | Rename the current session (`UpdateSession`). |
| `/keys` | List configured providers and provider IDs with saved credentials; never display key values. |
| `/key set <provider>`, `/login <provider>` | Open concealed entry for a provider API key; Enter saves, Esc cancels. |
| `/key remove <provider>` | Delete the provider's saved credential. |
| `/workflows`, `/workflow select <name>`, `/workflow run [name]` | View sources and selected state; select or start a Workflow in the selected session. |
| `/interactions` | View pending permissions and questions. |
| `/approve <id>`, `/deny <id>` | Respond to a permission request for this run only (`persist: false`). |
| `/answer <id> <text>` | Answer a question request. |
| `/cancel` or Esc | Cancel the running turn: the status line shows `Cancelling…`, then `Cancelled · Ready`. |
| `/refresh` or Ctrl+R | Reload sessions, messages, interactions, models, Workflows, and the command catalog (commands and skills). |
| `/sidebar [on\|off]` or Ctrl+B | Show or hide the sidebar. Without an argument it toggles what is visible now. |
| `/thinking [on\|off]` or Ctrl+O | Expand or collapse every reasoning (`Thinking`) block. |
| `/compact` | Compact the session's context now (`CompactSession`); the status line shows `Compacting…`, then `Compacted · <strategy>`. |
| `/summarize` | Summarize the session into a new message (`SummarizeSession`). |
| `/todos` | Show the session's todo list (`GetSessionTodo`) in the main panel. |
| `/status` | Show the server URL, backend version, directory, session, agent, model, and permission mode. |
| `/init`, `/review` | Server built-in commands from the backend command catalog, run as `CommandTurn`s. |
| `/<skill> [args]` | Run a discovered skill as a `CommandTurn` (see [Skill commands](#skill-commands)). |
| `/api` | List the HTTP operations from the generated OpenAPI catalog. |
| `/api METHOD /v1/path [JSON]` | Send a scoped HTTP/JSON request and show its JSON response. |
| `/help` | Show command help, including server and skill commands. |
| Tab | Complete a slash command name (or, in the command menu, the highlighted entry) or a supported argument; repeat Tab to cycle argument matches. |
| PgUp / PgDn | Scroll the transcript one page (the view height minus two rows). |
| Ctrl+Home / Ctrl+End | Jump to the top of the transcript / to the newest line, which the view then follows again. Plain Home / End do the same while the input is empty; with text in the input they move the cursor. |
| Mouse wheel | Scroll the transcript. |
| Click on a `Thinking` line | Expand or collapse that one reasoning block. |

`/sessions` also shows the sidebar when the terminal is too narrow for it, so
the list it refreshes is on screen.

The bottom instruction row is separate from the status message above the
input. Status updates and completion suggestions can change without erasing
the next-step instruction.

Other slash commands are forwarded to the backend as `CommandTurn`s, so
custom commands and skills from the server catalog (`ListCommands`, which
already includes skills tagged `source: "skill"`) remain usable in this
frontend. Tab suggestions and the command menu also use that catalog.
Argument completion covers agents, sessions, models, Workflows, pending
interaction IDs, provider IDs, saved key names, and HTTP operations from the
generated OpenAPI catalog. Suggestions are refreshed with `/refresh` or
Ctrl+R, and whenever the session or directory changes.

The API command accepts `GET`, `POST`, `PUT`, `PATCH`, and `DELETE`; the optional
body must be JSON. `GET` has no body. Include query parameters directly in the
path. For example:

```text
/api GET /v1/health
/api GET /v1/sessions
/api PATCH /v1/sessions/hysec_... {"title":"Review"}
```

It only accepts paths beginning `/v1/`, so a command cannot redirect the
client to another origin. The catalog marks server-streaming operations with
`[stream]`; the one-shot API command does not consume those streams. Session
SSE is connected automatically when a session is open. PTY WebSocket sessions
need a WebSocket client; the command view can still call their JSON setup
routes. See the [protocol guide](protocol/README.md) for those frames.

## Layout

```text
hya · <session> · <agent> <provider/model> · <server>   ┌─Sessions───────────┐
                                                        │▸ 1. Review         │
┃ your prompt                                           │   build            │
                                                        │                    │
● build · fake/model                                    └────────────────────┘
The reply, rendered as Markdown.                        ┌─Todos──────────────┐
                                                        │No todos yet        │
                                                        └────────────────────┘
┌─Pending (1)────────────────────────────────────────┐  ┌─Context────────────┐
│! bash · perm_…                                     │  │Session  hysec_…    │
│/approve <id> · /deny <id> · /answer <id> <text> · …│  │Agent    build      │
└────────────────────────────────────────────────────┘  │Model    fake/model │
Ready                                                   │Messages 2          │
┌────────────────────────────────────────────────────┐  │Dir      …/work     │
│ Message, /command, !shell, or @file                │  │Server   127.0.0.1:…│
└────────────────────────────────────────────────────┘  └────────────────────┘
Enter a prompt · /new creates a session · /help …
```

The main column holds, from top to bottom: the header line (session, agent,
model, server, in the accent color), the transcript (or the panel of the
current view: models, Workflows, keys, API, help), the pending block, the
status line, the bordered input, and the instruction line.

- **Sidebar.** Three titled boxes on the right: `Sessions` (the list; `▸`
  marks the open one), `Todos` (a placeholder until the todo panel lands),
  and `Context` (session, agent, model, projected message count, directory,
  server). It is 32 columns wide (at most 40% of a narrow terminal, at least
  20). By default it follows the width: shown at 110 columns or more, hidden
  below, so an 80-column terminal gets the full width for the transcript.
  Ctrl+B or `/sidebar` pins it shown or hidden at any width; `/sidebar on` and
  `/sidebar off` set it explicitly. The status line confirms the change
  (`Sidebar shown · Ctrl+B toggles`).
- **Pending block.** While permission requests (`!`) or questions (`?`) wait,
  a `Pending (N)` box appears above the status line with up to three of them
  (`! <title> · <id>`) and the commands that answer them. `/interactions`
  lists every detail. It disappears when nothing is pending.
- **Keys and the browser.** Ctrl+B and Ctrl+O are not reserved by browsers,
  so they also work in the WebUI (`packages/hya-tui-web`). Ctrl+B is tmux's
  default prefix; inside tmux press it twice (tmux passes the second one
  through) or use `/sidebar`. Ctrl+B would otherwise move the input cursor
  left; the Left arrow still does.
- **Focus.** The input keeps the keyboard focus. Mouse clicks (on the
  transcript, a `Thinking` line, or the sidebar) never move it (the renderer
  runs with `autoFocus: false`).

The colors are fixed in `src/theme.ts`:

| Name | Value | Used for |
| --- | --- | --- |
| `bg` | `#11151b` | Screen and transcript background. |
| `panel` | `#1c2530` | Boxes, user message blocks, code blocks, the input. |
| `fg` | `#e8edf3` | Text. |
| `muted` | `#9caab9` | Status line, instructions, `Thinking` lines, model names, queued prompts. |
| `accent` | `#73c8e8` | Header, user message bar, assistant name, headings, list markers. |
| `border` | `#405366` | Box borders and titles. |
| `error` | `#f07878` | Error notices and failed tool calls. |
| `warning` | `#e5c07b` | Length-limit and cancel notices. |

Code block tokens use `syntaxColors` (keyword `#c792ea`, string `#a5d6a7`,
number `#f78c6c`, comment `#7a8a9c`, function `#82aaff`, type `#ffcb6b`,
operator `#89ddff`) and inline code `#f2a97a`.

## Messages

Each message in the transcript is drawn by role:

- **User** prompts are panel-colored blocks with a heavy accent bar (`┃`) on
  the left. A queued prompt (see below) uses a muted bar and text and a
  `queued` tag on its right.
- **Assistant** messages start with a header, `● <agent> · <provider/model>`:
  the agent name in the accent color, the model muted. The v1 server does not
  fill `MessageInfo.agent` and `.model` yet, so the header shows the open
  session's agent and model. Then come the parts, in order, and at most one
  notice.

| Part or finish | Shown as |
| --- | --- |
| Text | Markdown (below). |
| Reasoning | One muted line, `▸ Thinking · N words` (`Thinking…` while it is the part still streaming). Expanded: `▾ Thinking · N words`, then the text in muted italics beside a bar. |
| Tool call | One muted line, `↳ <tool> · <state>` (`running`, `ok`, …); a failed call is `↳ <tool> · error: <message>` in the error color. When the command of a shell call is known, `$ <command>` follows, indented, and then its output (muted, at most 12 lines, then `… N more lines`). See [Shell turns](#shell-turns). |
| Attachment | `↳ attachment · <name>`. |
| `FINISH_REASON_STOP`, `FINISH_REASON_TOOL_CALLS` | Nothing: a normal finish is not noteworthy. |
| `FINISH_REASON_LENGTH` | `! Reply stopped at the output length limit` (warning color). |
| `FINISH_REASON_CANCELLED` | `! Cancelled` (warning color). |
| `FINISH_REASON_ERROR` or an `error` on the message | `✗ <code>: <message>` (error color), for example `✗ provider_error: http status 400: bad request`; `✗ Turn failed` when no error text was recorded. The error shows as soon as `errorReported` arrives. |

**Markdown.** Assistant text is rendered by OpenTUI's built-in `<markdown>`
renderable (`@opentui/core` 0.5.12): headings (accent, bold, `#` hidden),
**bold**, *italic*, strikethrough, `inline code`, links (the label followed by
the URL in parentheses, since terminals may not support hyperlinks), bullet
and numbered lists with nested indentation, task lists, block quotes (a bar on
the left), tables, horizontal rules, and fenced code blocks. A fenced block is
a panel-colored box with the language name on its first row; its tokens are
highlighted by tree-sitter in OpenTUI's parser worker. Highlighting covers the
grammars bundled with `@opentui/core`: TypeScript, JavaScript (and their JSX
variants), Markdown, and Zig. Other languages render as plain text on the
panel color. Nothing is downloaded at run time.

While a reply streams, the renderer keeps its last blocks provisional, so an
unclosed code fence shows its lines as code so far and an unclosed `**` shows
as plain text until it closes. When the reply finishes, its final text is
parsed again from the start.

**Reasoning.** Reasoning parts arrive as `reasoning` parts (durable deltas;
see the protocol guide). They are collapsed by default. Ctrl+O or `/thinking`
expands or collapses all of them (and forgets per-block choices); a click on
one `Thinking` line toggles just that block. The word count is the reasoning
text split on white space. Only provider routes that stream reasoning produce
these parts (for example `openai-response`; the `openai-compatible` decoder
ignores reasoning).

**Scrolling.** The transcript follows the newest line while you are at the
bottom. Scroll up (PgUp, the mouse wheel, Ctrl+Home) and it stays where you
left it; when more content arrives below, a `↓ New messages below · End
jumps` hint appears at the bottom right. End (with an empty input), Ctrl+End,
or scrolling back to the bottom clears the hint and resumes following.
Submitting a prompt jumps to the bottom. Opening a session starts at its
bottom. The transcript shows the newest 200 messages.

## Streaming, queued prompts, and turn status

The assistant reply appears chunk by chunk while the model streams it. When
the reply is complete, the transcript shows the server's stored copy of it;
the text does not repeat or flicker when that happens.

You can type the next prompt while a turn is running. Press Enter and the
prompt appears dimmed at the end of the transcript, tagged `queued`. The status line counts the waiting prompts
(`Running · msg_… · 1 queued`). When the running turn ends, the frontend sends
the oldest queued prompt; several queued prompts go one per turn, in the
order you typed them. The server has no prompt queue of its own. It rejects
a prompt with `409 session_busy` while a turn runs, and it releases the
session shortly after the reply finishes. So the frontend retries a busy
prompt a few times with a short backoff (about 100 ms growing to 1 s). If the
session is still busy after that (for example, another client started a
turn), the prompt stays queued and is sent after the next turn end seen on the
stream. Opening another session drops the queued prompts of the previous one.
A queued prompt is still sent after a cancelled or failed turn.

The status line above the input shows the turn state:

| Status | Meaning |
| --- | --- |
| `Sending prompt…` | The prompt is being admitted (`CreateTurn` in flight). |
| `Session busy · retrying (N)` | The server answered `409 session_busy`; the prompt is retried. |
| `Running · <turn id>[ · N queued]` | The turn runs; `N` prompts wait. |
| `Session busy · N queued prompt(s) wait(s) for the running turn` | Retries ran out; the prompts wait for the next turn end. |
| `Ready` | The turn finished. `Ready · reply stopped at the length limit` when the model hit its output limit. |
| `Cancelling…` | Esc or `/cancel` sent `CancelTurn`; the turn has not ended yet. |
| `Cancelled · Ready` | The turn was cancelled (Esc or `/cancel`). |
| `Running shell · <command>` | A `!command` shell turn runs. |
| `Press Ctrl+C again to quit` | The first Ctrl+C; it goes back to the previous status after 2 s. |
| `Error · <code>: <message>` | The turn failed, for example `Error · provider_error: http status 400: …`. `Error · turn failed` when the backend recorded no error text. |

A failed assistant message also shows its error in the transcript, as a line
under its header, in the error color:

```text
● build · openai/gpt-5
✗ provider_error: http status 400: bad request
```

## Composer

The input at the bottom of the main column is a multi-line editor (OpenTUI's
built-in `<textarea>`). It keeps the keyboard focus. Its placeholder is
`Message, /command, !shell, or @file`.

**Writing.** Enter sends the whole input: a prompt, a `/command`, or a
`!command`. Ctrl+J inserts a newline in every terminal and in the WebUI;
Alt+Enter does too. Shift+Enter inserts a newline only in terminals that
report it as a separate key (the kitty keyboard protocol, which OpenTUI
requests at startup, or modifyOtherKeys). xterm.js, and so the
WebUI, sends a plain Enter for Shift+Enter, so there it sends the input. A
bracketed paste inserts its text, line breaks included, and never sends it.
The box grows with its content up to 8 rows (wrapped lines count), then
scrolls. Newlines stay in the prompt text, so the transcript shows the lines
as typed. Editing keys: Left/Right, Up/Down between lines, Home/End to the
start/end of the current line, Ctrl+Left/Right or Alt+Left/Right by word,
Ctrl+A / Ctrl+E to the start/end of the logical line, Backspace, Delete,
Alt+Backspace deletes the previous word (Ctrl+W too, outside a browser, which
reserves it), Ctrl+U / Ctrl+K delete to the line start/end, Ctrl+- undo.

**History.** Every sent input (prompts, `!commands`, `/commands`) is kept for
the life of the TUI process, up to 200 entries; it is not saved to disk.
Up on the first line of the input shows the previous entry; Down on the last
line shows the next one, and past the newest entry it restores what you were
typing before. Any edit ends history navigation. Repeated sends of the same
input are stored once.

**Esc.** Esc closes the file list if it is open. Otherwise, while a turn
admitted by this TUI runs, it cancels that turn (like `/cancel`): the status
shows `Cancelling…`, then `Cancelled · Ready`, and the transcript shows
`! Cancelled`. Text you typed meanwhile stays. With no turn running, Esc
clears the input.

**Quitting.** The renderer does not quit on Ctrl+C by itself. The first
Ctrl+C clears the input (on an empty input it only arms) and shows
`Press Ctrl+C again to quit`; a second Ctrl+C within 2 s quits. Any other key
in between disarms it. Ctrl+D on an empty input quits; with text it deletes
the character under the cursor. `/exit` and `/quit` quit. Quitting destroys
the renderer, which restores the terminal, and exits with code 0.

Example:

```text
explain these two functions:          ← Ctrl+J
- parse_args                          ← Ctrl+J
- run                                 ← Enter sends all three lines
```

### Shell turns

An input that starts with `!` runs the rest of the line as a shell command in
the open session (a session is created first if none is open). The input box
shows the shell mode while you type: its border turns the warning color and
its title reads `! shell`. The command goes through the prompt queue like a
prompt, so it waits while a turn runs.

The backend runs it as a `ShellTurn`: its builtin `bash` tool runs the command
in the session's working directory, with no model round, under the session's
agent and permission rules. The default permission policy asks before `bash`
runs, so a pending request may appear; answer it with `/approve <id>`.
`CreateTurn` returns only when the command has finished; meanwhile the status
reads `Running shell · <command>`.

The backend records the turn as two messages: a user message with the fixed
text `The following tool was executed by the user`, and an assistant message
with one `bash` tool call. The transcript shows the user message as
`!<command>` and the tool call with the command below it:

```text
┃ !echo hello

● build · openai/gpt-5
↳ bash · ok
  $ echo hello
  hello
```

The command comes from this TUI's own shell turns, or from the tool call's
`inputJson` (`{"command": …}`) when the server fills it. The output line
needs `ToolCallPart.outputJson`; servers that leave it empty show only the
command. Esc cancels a running shell command; the turn then reads
`Cancelled · Ready`.

### File references

Type `@` and at least one character (at the start of the input or after a
space) to see up to 8 files and directories under `--dir` whose relative
path contains the text. The list is a `Files` box above the input; the
selected row is marked `▸` in the accent color. Up/Down move the selection;
Tab or Enter replaces the `@text` token with `@<relative path>` and a space;
Esc closes the list until you edit the token again. The lookup runs 120 ms
after the last keystroke. Matching is case-sensitive (the server's glob), and
the best matches come first: file name starts with the text, then file name
contains it, then only the path does; shorter paths first. Slash-command lines
(`/…`) have no file references.

The reference is plain text: the prompt carries `@src/main.rs` as typed, and
nothing is attached (`PromptTurn` is text only). The agent reads the file
with its tools if it needs it.

### Command menu

Typing `/` at the start of the input (before any other character) opens a
`Commands` box above it, the same overlay position and key handling as the
`@file` list. Each row shows the name, its argument hint, its description
(truncated to width), and its source in brackets: `[local]` (this TUI's own
registry), `[command]` (a custom or built-in server command, `/init` and
`/review` among them), or `[skill]` (a discovered skill — see
[Skill commands](#skill-commands) below). The list is fuzzy-filtered as you
keep typing the name: an exact match ranks first, then a prefix match, then a
substring match, then any name whose letters appear in order (a subsequence
match); ties break alphabetically. Up/Down move the highlight; Esc closes the
menu and keeps the typed text.

Tab always completes the highlighted name and a trailing space, so you keep
typing its arguments. Enter's behavior depends on the highlighted command's
argument hint: with no hint, or one written `[in brackets]` (an optional
argument, for example `/new [agent] [model]` or `/sidebar [on|off]`), Enter
runs the command as is. Any other hint (`/open <id|number>`, `/key
set|remove <provider>`) names a required first argument, so Enter behaves
like Tab: it completes the name and waits for you to type the argument.

Local and backend (command or skill) names are merged and deduplicated by
name; a local name always wins a clash with a backend name (the registry
looks up local commands before falling back to the backend, so a local
command is what actually runs either way). The list refreshes with
`/refresh`/Ctrl+R and whenever the session or directory changes, the same as
Tab completion.

### Skill commands

A discovered skill runs as `/<skill> [args]`, the same as any other backend
command: the TUI sends `{command: {command, arguments}}` (`CommandTurn`);
the backend catalog (`crates/hya-server/src/support/command_catalog.rs`)
resolves the name against custom commands and skills together, expands the
skill's template with the arguments (`$1`, `$ARGUMENTS`) server-side, and
runs the result as a normal prompt turn. The transcript shows what you typed,
`/<skill> args`, in place of the backend's expanded prompt text — the same
idea as a `!command` shell turn showing `!<command>` (see
[Shell turns](#shell-turns)) — then the agent's streamed reply as usual.
`CreateTurn` returns the user message id as the turn id for a command turn
(unlike a shell turn), so the TUI remembers the typed `/name args` by that id
(`state/store.ts` `commandDisplay`, `state/messages.ts`
`commandUserView`) and shows it once the projection carries that message.

## Interface definitions

The frontend uses the existing HTTP/JSON+SSE transport. Every request carries
`x-hya-directory: <absolute --dir path>`; JSON uses protojson lower camel case,
string encoded 64-bit values, and the error envelope documented in the
[protocol guide](protocol/README.md). These are the first-class calls:

| Method and route | Request | Response read by the TUI |
| --- | --- | --- |
| `GET /v1/bootstrap` | No body | `Bootstrap` (`location`, `agents`, `models`, `interactions`) |
| `GET /v1/sessions` | No body | `ListSessionsResponse.sessions: SessionInfo[]` |
| `POST /v1/sessions` | `{agent: string, model: string, workdir: string}` | `CreateSessionResponse.session: SessionInfo` |
| `GET /v1/sessions/{id}` | No body | `SessionInfo` (including `permissionMode`, read by `/status`) |
| `PATCH /v1/sessions/{id}` | `{title?: string, model?: string, agent?: string}` (`UpdateSession`; `/model`, `/agent`, `/rename` each send one field) | `SessionInfo` |
| `GET /v1/sessions/{id}/messages` | No body | `ListMessagesResponse.messages: MessageInfo[]` |
| `POST /v1/sessions/{id}/compact` | `{}` (`CompactSession`) | `CompactSessionResponse {compactedUntilSeq, strategy}` for `/compact` |
| `POST /v1/sessions/{id}/summarize` | No body (`SummarizeSession`) | `SummarizeSessionResponse {summaryMessage}` for `/summarize` |
| `GET /v1/sessions/{id}/todo` | No body (`GetSessionTodo`) | `TodoList.items: TodoItem[]` for `/todos` |
| `POST /v1/sessions/{id}/turns` | `{prompt: {text: string}}` | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{command: {command: string, arguments: string}}` for other slash commands | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{shell: {command: string, agent: string, model?: {providerId: string, modelId: string}}}` for `!command` (the session's agent and model) | `CreateTurnResponse.turn: TurnInfo` once the command has finished; `id` is the shell turn's assistant message. |
| `POST /v1/sessions/{id}/turns/{turn}/cancel` | `{}` | `TurnInfo`. Esc and `/cancel` send the admitted turn id (the user message id). The server cancels whatever runs in the session, so a shell turn whose id is not known yet is sent as `current`. |
| `GET /v1/fs/find?pattern=**/*<text>*&limit=50` | No body (`FindFiles`, scoped by `x-hya-directory`) | `FindFilesResponse.paths: string[]` (relative paths) for `@file` suggestions. |
| `GET /v1/sessions/{id}` | No body | `SessionInfo.lastSeq` when a session is opened (the stream's first `sinceSeq`). |
| `GET /v1/sessions/{id}/events/stream?sinceSeq=N` | SSE | `StreamFrame` with `event` or `resync`; `N` is the last applied durable seq. |
| `GET /v1/sessions/{id}/events?sinceSeq=N&limit=500` | No body | `ListEventsResponse.events` / `nextSeq`, paged, to fill the gap after each stream (re)connect and `resync`. |
| `GET /v1/interactions` | No body | `ListInteractionsResponse.interactions: Interaction[]` |
| `POST /v1/interactions/{id}/respond` | `{permission: {allowed: boolean, persist: false}}` or `{question: {answer: string}}` | `RespondInteractionResponse.applied` |
| `GET /v1/models` | No body | `ListModelsResponse.models: ModelSummary[]` |
| `GET /v1/providers` | No body | `ListProvidersResponse.providers: ProviderSummary[]` for key suggestions. |
| `GET /v1/commands` | No body | `ListCommandsResponse.commands: CommandSummary[]` (includes skills, tagged `source: "skill"`) for slash completion and the command menu. |
| `GET /v1/auth` | No body | `ListProviderAuthResponse.providerIds: string[]` (saved provider IDs only; empty field omitted). A 404 marks key listing unavailable without blocking startup. |
| `PUT /v1/auth/{provider_id}` | `{apiKey: string}` | `SetProviderAuthResponse.status: AuthStatus`; key value is sent only to the backend. |
| `DELETE /v1/auth/{provider_id}` | No body | `RemoveProviderAuthResponse` (empty). |
| `GET /v1/workflows` | No body | `ListWorkflowsResponse.workflows: WorkflowSummary[]` |
| `GET /v1/sessions/{id}/workflow` | No body | `WorkflowState` |
| `POST /v1/sessions/{id}/workflow` | `{select: {name: string}}` or `{run: {name: string}}` | `SubmitWorkflowCommandResponse` |

The one-row footer sits directly below the input panel. Its content is selected
from the current view; it makes no HTTP request:

| View or state | Bottom instruction |
| --- | --- |
| Chat | `Enter a prompt · /new creates a session · /help lists commands · / opens the command menu` |
| Models | `Next: /model <provider/model> to switch this session · /help` |
| Workflows | `Next: /workflow select <name> or /workflow run [name]` |
| Interactions | `Next: /approve <id>, /deny <id>, or /answer <id> <text>` |
| Saved keys | `Next: /key set <provider> to add · /key remove <provider> to delete · Tab completes` |
| Saved keys when `GET /v1/auth` is unavailable | `Next: restart backend 0.41.0+ to list saved keys · /help` |
| Concealed key entry | `Paste API key · Enter saves · Esc cancels` |
| API | `Next: /api GET /v1/health · /help for command syntax` |
| Help | `Enter a prompt or choose a /command · Tab completes` |
| Todos | `Next: /refresh to reload the list · /help` |
| Status | `Next: /model, /agent, or /rename to change what's shown · /help` |

### Stream frames and the transcript

The server projection (`ListMessages`, `MessageInfo.parts`) is the
authoritative transcript. Stream frames feed a transient overlay that shows
what the projection cannot show yet, mainly the live text of the in-flight
round. The overlay is never persisted and is rebuilt from the stream. The
rules follow the protocol guide's
[Live and durable frames](protocol/README.md#live-and-durable-frames):

| Frame (`StreamEvent` field) | Kind | Effect in the TUI |
| --- | --- | --- |
| `messageStarted {message, role}` | durable | Overlay message with its role; projection re-read (debounced: 120 ms after the last such frame, but at least every 400 ms while frames keep coming). |
| `partStarted {message, part, kind}` (`text`, `reasoning`) | live or durable | Overlay part. A part id the overlay already has is not a new part. |
| `partAppended {message, part, textDelta}` | live (assistant text) or durable (reasoning, tool arguments, user text) | Appends `textDelta` to the part. No projection re-read. |
| `partReplaced {message, part, text}` | live (plugin rewrite) or durable (end of round) | Sets the part's whole text, replacing the live deltas. |
| `partCompleted {message, part}` | live or durable | No overlay change; a durable one triggers a projection re-read. |
| `errorReported {message, code, errorMessage}` | durable | Stored as the message's error. Shown in the transcript and, at turn end, in the status line. |
| `messageFinished {message, finish, cause}` | durable | The turn ends at the first assistant `messageFinished` after the turn's user message whose `finish` is not `FINISH_REASON_TOOL_CALLS`. Then the projection is re-read. |
| `permissionRequested`, `questionRequested`, `interactionResolved` | live | Pending list re-read. |
| `resync {lastSeq}` | — | Live parts that were mid-stream stop taking deltas until their durable `partReplaced`; `ListEvents` fills the gap; the projection is re-read. |

- **Sequence numbers.** The client keeps the last applied durable `seq` as a
  decimal string and compares with `BigInt`, so 64-bit values stay exact. A
  durable frame at or below it is a duplicate and is ignored. Live frames have
  no `seq` and are always applied.
- **(Re)connect.** The stream is subscribed with
  `sinceSeq = last applied seq`. Before any frame is read, `ListEvents` pages
  are replayed through the same fold. The stream does not replay history, so
  this fills the gap. After a reconnect, the projection is re-read too. A
  prompt is admitted only once the stream is subscribed (up to 3 s wait), so
  no frame of its turn is missed.
- **Handover.** The displayed transcript is the projection with the overlay
  merged by message id and part id. The overlay's text wins for a part that is
  still streaming. Overlay parts and messages that are not in the projection
  yet follow the projected ones. A projected message with a `finish` is shown
  exactly as projected, and the overlay drops it in the same store update. The
  live text and the durable text are identical, so the handover does not
  flicker. Projection reads complete in order: an older read never replaces a
  newer one.
- **Turn id.** `CreateTurn` returns the user message id as `TurnInfo.id`. It
  is used for `/cancel` and to find the turn's end. A reply that finishes
  before `CreateTurn` returns ends the turn as soon as the response arrives.
- **Session switch.** Opening a session aborts the old stream and resets the
  overlay, the prompt queue, and the turn state. Frames of any other session
  are ignored.
- **Rendering cost.** Frames are folded at once, but the overlay is published
  to the store at most once per 16 ms. A fast delta stream therefore renders
  about once per display frame, not once per chunk. Each message's view model
  (`state/messages.ts`) is cached per message object; projected messages and
  unchanged overlay messages keep their identity, so a delta rebuilds only the
  view of the message it changed. Messages and their parts are components
  keyed by id: a delta updates the existing Markdown renderable of that part
  instead of recreating it.

List requests follow the server's `page.nextCursor` using the
`page.cursor` and `page.limit` query keys. `GET /v1/auth` is an unpaginated
names-only list. The generic `/api` command sends the supplied JSON unchanged to
the named `/v1` route; its full request and response schemas are in the
[generated API reference](protocol/api-reference.md).
For non-2xx responses with an empty or invalid JSON body, the frontend reports
`METHOD /v1/path: HTTP <status> <status text>`; a structured error envelope
continues to show its code and message.

## Code layout

The frontend is written with [`@opentui/solid`](https://github.com/anomalyco/opentui)
(Solid JSX over `@opentui/core`). `@opentui/core`, `@opentui/solid`, and
`solid-js` are pinned to exact versions in `package.json` and must move
together.

| Path | Role |
| --- | --- |
| `src/main.ts` | Entry. Registers the Solid JSX transform (`@opentui/solid/preload`), parses flags, then dynamically imports the app. |
| `src/cli.ts` | `--server`, `--dir`, `--help` parsing and the usage line. |
| `src/client.ts` | Typed v1 HTTP/JSON+SSE client (`HyaClient`, `SseDecoder`, `parseApiCommand`). |
| `src/state/store.ts` | `createAppStore()`: the single store. It holds the server projection (sessions, messages, interactions, models, agents, providers, workflows, saved key names, backend commands, todos, stream cursor), the published streaming overlay, the prompt queue, the turn state (`running`, `turnId`), and UI state (view, status, key-entry provider and mask, sidebar mode, terminal columns, the reasoning switch and per-part toggles, the jump-to-bottom tick, the `/status` text, the backend version from bootstrap, the `/name args` display text of command turns by user message id). Each field is a Solid signal, and only the store's mutation methods change it. |
| `src/state/overlay.ts` | `TranscriptOverlay`: the pure fold of stream frames by message and part id (seq filter, live/durable handover, `resync` handling, turn-end lookup). `mergeTranscript()` merges it over the projection. |
| `src/state/messages.ts` | The transcript view model: `transcriptViews()` (projection + overlay + waiting queued prompts), `messageView()` (role, agent/model, typed blocks, finish notice; cached per message object), `finishNotice()`, `reasoningLabel()`, `reasoningExpanded()`. |
| `src/state/layout.ts` | Sidebar rules: `layoutBreakpoints`, `sidebarVisible()`, `toggledSidebar()`, `sidebarWidth()`, and `parseSwitch()` for `on`/`off` arguments. |
| `src/state/scroll.ts` | `ScrollFollow` (the "new messages below" hint), `atBottom()`, `pageStep()`. |
| `src/state/format.ts` | Pure text for the header, sidebar (session list, context box), pending lines, and the non-chat views. |
| `src/app/controller.ts` | `createController()`: refreshes, the session SSE loop (subscribe, `ListEvents` gap-fill, `resync`), batched overlay flushes, the debounced projection re-read (`app/debounce.ts`), session creation, prompt submission, command dispatch, and concealed key entry. It writes results into the store. |
| `src/app/turns.ts` | `createTurnRunner()`: the client-side prompt queue, `409 session_busy` retry, and turn-end detection and status text. |
| `src/app/App.tsx`, `src/app/run.tsx`, `src/app/context.ts` | Root layout (main column + sidebar), renderer startup, and the `AppContext` (store, controller, server URL, and `ui` handles such as the transcript's scroll actions) that components read with `useApp()`. |
| `src/components/` | `Header`, `MainPanel` (transcript or view panel), `Transcript` (scrollbox, follow/hint), `MessageView` (`MessageItem`, user/assistant messages, blocks, reasoning, tool lines with shell command and output, `KeyedFor`), `Markdown` (the `<markdown>` wrapper, `SyntaxStyle`, code-block boxes), `Panel`, `PendingBlock`, `Sidebar`, `StatusLine`, `Composer` (the `<textarea>` editor, its height, history, Esc / Ctrl+C / Ctrl+D, the shell-mode border, the `@file` list, the `/` command menu, Tab completion, key actions, concealed key entry), `Footer`. |
| `src/composer/` | Pure composer logic: `history.ts` (`InputHistory`), `quit.ts` (`createQuitGuard`, the Ctrl+C double press), `escape.ts` (`escapeAction`), `shell.ts` (`shellCommand`, `isShellInput`), `mention.ts` (`mentionAt`, `insertMention`, `findPattern`, `rankPaths`). |
| `src/commands/` | The slash-command registry (`registry.ts`), the built-in commands (`native.ts`), the `/help` text (`help.ts`), and the command menu's merge/fuzzy-filter/argument-hint logic (`menu.ts`: `mergeCommandEntries`, `filterCommands`, `requiresArgument`). |
| `src/keys/bindings.ts` | The global key binding table (`keyBindings`) and the textarea overrides (`composerKeyBindings`: Enter submits; Ctrl+J, Shift+Enter, Alt+Enter insert a newline; Home/End). |
| `src/completion.ts`, `src/instructions.ts`, `src/api.ts`, `src/theme.ts` | Tab completion and `SecretEntry`, footer instructions, the OpenAPI operation catalog, and the palette (`colors`, `syntaxColors`, and `syntaxStyles`, the Markdown/tree-sitter scope styles). |

The Solid transform has two parts. `bunfig.toml` preloads
`@opentui/solid/preload` for `bun test` and for `bun src/...` run inside the
package. `tsconfig.json` sets `"jsx": "preserve"` and
`"jsxImportSource": "@opentui/solid"`. Bun reads `bunfig.toml` only from the
directory it runs in, so `src/main.ts` imports the preload itself. It then
loads `.tsx` modules and `solid-js` with a dynamic `import()`. Keep static
imports in `main.ts` free of Solid code. Without the preload, Bun resolves
`solid-js` to its non-reactive server build.

To add a slash command, add a `CommandSpec` to `nativeCommandSpecs` in
`src/commands/native.ts`:

```ts
{
  name: "/title",
  description: "Rename the current session",
  argumentHint: "<text>",
  complete: ({ words, current, head }, context) => [],   // optional argument completion
  run: async ({ store, client, actions }, { args, argumentsText }) => { /* … */ },
}
```

The name becomes Tab-completable and appears in the command menu
automatically (source `[local]`; it wins a name clash with a backend
command). An `argumentHint` written `[in brackets]` is optional (the command
menu's Enter runs it as is); anything else is treated as required (Enter
completes the name and waits). Add a line to `src/commands/help.ts` and a row
to the command table above. Unregistered `/names` still go to the backend as
`CommandTurn`s (custom commands and skills; see
[Skill commands](#skill-commands)). To add a key, append a
`KeyBinding` to `src/keys/bindings.ts` and handle its action in
`components/Composer.tsx`. A binding's `matches(key, context)` may depend on
`context.composerEmpty` (plain Home/End scroll only while the input is
empty). Do not bind a core action only to a
browser-reserved shortcut (see `docs/tui-web.md`). The renderer runs with
`exitOnCtrlC: false`; Ctrl+C is the composer's `quit` action (the double
press in `src/composer/quit.ts`). Editing keys of the input are
`composerKeyBindings`, merged over OpenTUI's textarea defaults by key.

## Verify locally

```sh
cd packages/hya-tui
bun install --frozen-lockfile
bun run typecheck
bun test
```

Then check the rendered TUI in the browser from `packages/hya-tui-web`
(`bun run typecheck && bun test ./test && bunx playwright test`; see
[tui-web.md](tui-web.md)). `e2e/hya-tui.spec.ts` and
`e2e/hya-tui-commands.spec.ts` cover the layout, colors, commands, key
entry, narrow widths, and Ctrl+C. `e2e/hya-tui-layout.spec.ts` covers the
main column and sidebar at the default viewport and at about 80 columns
(Ctrl+B, `/sidebar`) and the pending block. `e2e/hya-tui-messages.spec.ts`
covers user and assistant styling, Markdown and code highlighting, reasoning
(Ctrl+O, `/thinking`, click), error, length, and cancel notices, and
scrolling (PgUp/PgDn, End, Ctrl+End, the wheel, the new-messages hint).
`e2e/hya-tui-streaming.spec.ts` uses the fake model to cover streaming text,
queued prompts, and the turn status line (`Ready`, provider errors).
`e2e/hya-tui-commands-menu.spec.ts` covers the `/` command menu (open,
fuzzy filter, sources, Up/Down, Tab, Esc, Enter's argument-hint rule), skill
commands (a fixture `SKILL.md` under `.hya/skills/<name>/`), `/compact`,
`/rename`, and `/status`. `e2e/hya-tui-composer.spec.ts` covers the composer:
Ctrl+J / Alt+Enter newlines and box growth up to 8 rows, Shift+Enter in the
browser, bracketed paste, cursor editing, history, Esc (clear, and cancel of
a hanging fake-model turn), Ctrl+C once and twice, Ctrl+D, `/exit`,
`!echo hello`, and `@file`
suggestions at the default width and about 80 columns.
