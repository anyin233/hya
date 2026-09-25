# OpenTUI frontend

The `packages/hya-tui` frontend is a basic terminal client for a running
`hya serve` process. It uses OpenTUI for display and input while the
backend remains the owner of sessions, event history, tool execution, and
permissions. The screen is one main column (the transcript of the open
session, pending interactions, the status line, and the input) plus a
sidebar with the session list, todos, and session context that you can show
or hide (see [Layout](#layout)). Assistant replies render as Markdown with
highlighted code blocks; reasoning is collapsed to one `Thinking` line; each
tool call is a card with its state, a one-line summary, and an expandable
body; a subagent's `task` card shows the child's status and opens its session
read-only (see [Messages](#messages)). When the agent or one of its subagents
needs a permission decision or asks a question, a prompt docked above the
input shows the call and its options; press `1`, `2`, or `3` (see
[Permission and question prompts](#permission-and-question-prompts)).
Shift+Tab or `/permissions` switches the session's permission mode
(`manual`, `yolo`, or a mode an installed bundle provides); the status bar
shows the mode in effect (see [Permission modes](#permission-modes)).
Models, Workflows, and saved provider keys have
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
| `1` `2` `3`, Up/Down + Enter | With a permission prompt shown and an empty input: Allow once, Always allow, Deny. On a question prompt the digits pick its options (see [Permission and question prompts](#permission-and-question-prompts)). |
| Esc | Close the command menu or the file list; else, with a prompt shown and an empty input, deny the permission / reject the question; else, in a subagent's read-only view, return to the parent session; else cancel the running turn; else clear the input. |
| Ctrl+C | Clear the input and show `Press Ctrl+C again to quit`; a second Ctrl+C within 2 s quits. |
| Ctrl+D | Quit when the input is empty (otherwise delete the character under the cursor). |
| `/exit`, `/quit` | Quit. |
| `/new [agent] [model]` | Create a session in `--dir`, using the first visible agent and its model by default. |
| `/sessions` | Open the sessions picker: a `New session` row, then every session (subagent sessions nested under their parent); Enter opens, F2 renames, Ctrl+D deletes with confirmation (see [Pickers](#pickers)). |
| `/open <id or number>` | Switch sessions directly. Numbers count in the sidebar's order (subagent sessions under their parent). Opening a subagent's session shows it read-only (see [Subagents](#subagents)). |
| `/models`, `/model [provider/model]` | View catalog, or open the model picker (rows tagged by provider); `/model <provider/model>` switches directly. With no session yet, a picker or direct choice is remembered for the next one (see [Pickers](#pickers)). |
| `/agent [name]` | Open the agent picker (visible agents, tagged with their default model); `/agent <name>` switches directly. With no session yet, the choice is remembered for the next one. |
| `/rename <title>` | Rename the current session (`UpdateSession`); see also the sessions picker's F2 (see [Session titles](#session-titles)). |
| `/permissions [mode]` | Open the permission mode picker, or with a mode id switch to it directly (see [Permission modes](#permission-modes)). |
| Shift+Tab | Switch to the next permission mode: `manual` → `yolo` → bundle modes → `manual`. Switching to `yolo` asks for a confirmation the first time. In an open list (the command menu, the file list, a picker) it moves the highlight up instead. |
| `/keys` | List configured providers and provider IDs with saved credentials; never display key values. |
| `/key set <provider>`, `/login <provider>` | Open concealed entry for a provider API key; Enter saves, Esc cancels. |
| `/key remove <provider>` | Delete the provider's saved credential. |
| `/workflows`, `/workflow select <name>`, `/workflow run [name]` | View sources and selected state; select or start a Workflow in the selected session. |
| `/interactions` | View pending permissions and questions. |
| `/approve <id>`, `/deny <id>` | Respond to a permission request for this run only (`persist: false`); the keyboard fallback of the prompt, which shows the id. |
| `/answer <id> <text>` | Answer a question request. |
| `/cancel` or Esc | Cancel the running turn: the status line shows `Cancelling…`, then `Cancelled · Ready`. |
| `/refresh` or Ctrl+R | Reload sessions, messages, interactions, models, Workflows, and the command catalog (commands and skills). |
| `/sidebar [on\|off]` or Ctrl+B | Show or hide the sidebar. Without an argument it toggles what is visible now. |
| `/thinking [on\|off]` or Ctrl+O | Expand or collapse every reasoning (`Thinking`) block. |
| `/tools [on\|off]` or Ctrl+G | Expand or collapse every tool call card (see [Tool calls](#tool-calls)). |
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
| Click on a tool card | Expand or collapse that one card; on a `task` card, open the subagent's session read-only. |

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
mode manual · …/work · ⎇ main                           │▸ 1. Review         │
                                                        │   build · ◌ waiting│
┃ your prompt                                           │                    │
                                                        └────────────────────┘
● build · fake/model                                    ┌─Todos──────────────┐
◌ bash  cargo test · awaiting approval                  │○ write tests       │
⠹ 0:07 · Running bash cargo test · Esc to interrupt     │◐ fix the bug       │
                                                        └────────────────────┘
┌─Permission─────────────────────────────────────────┐  ┌─Context────────────┐
│bash  cargo test                                    │  │Session  hysec_…    │
│asked by build                                      │  │Agent    build      │
││ $ cargo test                                      │  │Model    fake/model │
│▸ 1  Allow once                                     │  │Messages 2          │
│  2  Always allow  bash: cargo test                 │  │Dir      …/work     │
│  3  Deny                                           │  │Server   127.0.0.1:…│
│1-3 or ↑↓ Enter · Esc denies · perm_… · mode manual │  └────────────────────┘
└────────────────────────────────────────────────────┘
Connected to hya 0.41.0 · /help for commands
┌────────────────────────────────────────────────────┐
│ Message, /command, !shell, or @file                │
└────────────────────────────────────────────────────┘
Enter a prompt · /new creates a session · /help …
```

The main column holds, from top to bottom: the header line (session, agent,
model, server, in the accent color), the status bar (permission mode,
directory, git branch, a compact todo count while the sidebar is hidden,
connection state — see
[Working indicator, status bar, and todo panel](#working-indicator-status-bar-and-todo-panel)),
the transcript (or the panel of the current view: models, Workflows, keys,
API, help), the working indicator while a turn this client admitted runs,
the pending block (asks of other sessions), the permission or question
prompt, the one-line yolo confirmation while it is asked, the status line,
the bordered input, and the instruction line. The permission mode picker
(`/permissions`) is drawn over the screen near the top while it is open.

- **Sidebar.** Three titled boxes on the right: `Sessions` (the list; `▸`
  marks the open one; a subagent's session is one `↳ N. <agent>` line nested
  under its parent, `· running` while it works, `· ◌ waiting` while a
  permission or question of that session waits for an answer), `Todos` (the
  live todo list — see
  [Working indicator, status bar, and todo panel](#working-indicator-status-bar-and-todo-panel)),
  and `Context` (session, agent, model, the merged transcript's message
  count, directory, server). It is 32 columns wide (at most 40% of a narrow
  terminal, at least 20). By default it follows the width: shown at 110
  columns or more, hidden below, so an 80-column terminal gets the full
  width for the transcript. Ctrl+B or `/sidebar` pins it shown or hidden at
  any width; `/sidebar on` and `/sidebar off` set it explicitly. The status
  line confirms the change (`Sidebar shown · Ctrl+B toggles`).
- **Prompt.** A pending permission request or question of the open session
  or one of its subagent sessions is a prompt box (warning-colored border)
  above the status line; see
  [Permission and question prompts](#permission-and-question-prompts).
- **Pending block.** While permission requests (`!`) or questions (`?`) of
  *other* sessions wait (sessions not in the open session's tree), a
  `Pending (N)` box appears above the prompt with up to three of them
  (`! <title> · <id>`) and the commands that answer them; open that session
  to get its prompt. `/interactions` lists every detail. It disappears when
  nothing else is pending.
- **Keys and the browser.** Ctrl+B, Ctrl+O, and Ctrl+G are not reserved by
  browsers, so they also work in the WebUI (`packages/hya-tui-web`). Ctrl+B is tmux's
  default prefix; inside tmux press it twice (tmux passes the second one
  through) or use `/sidebar`. Ctrl+B would otherwise move the input cursor
  left; the Left arrow still does.
- **Focus.** The input keeps the keyboard focus. Mouse clicks (on the
  transcript, a `Thinking` line, a tool card, or the sidebar) never move it (the renderer
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

Tool cards add `toolColors.done` `#a5d6a7` (the ✓ of a finished call and an
idle or done subagent) and `diffColors`: added rows `#a5d6a7`, removed rows
`#f07878`, hunk and file headers `#82aaff`, context rows `#9caab9` (muted).
A running spinner uses `accent`, a failed call `error`, a call waiting for a
permission answer `warning`, a pending one `muted`.

Code block tokens use `syntaxColors` (keyword `#c792ea`, string `#a5d6a7`,
number `#f78c6c`, comment `#7a8a9c`, function `#82aaff`, type `#ffcb6b`,
operator `#89ddff`) and inline code `#f2a97a`.

## Working indicator, status bar, and todo panel

**Working indicator.** While a turn this client admitted runs, one muted
line sits below the transcript, above the pending block and the
permission/question prompt dock (so the dock a pending ask needs still gets
the last word before the input): a spinner, the elapsed time (`m:ss`, or
`h:mm:ss` past an hour), the current activity, an optional `Queued N`, and
`Esc to interrupt`. The activity, highest priority first:

| Activity | When |
| --- | --- |
| `Waiting for approval` / `Waiting for an answer` | A permission or question prompt of the open session's tree is pending (the same ask the prompt dock shows). |
| `Waiting for subagent <agent>` | The streaming message's last block is a `task` card whose child session is starting or running, with no ask of its own yet. |
| `Running <tool> <summary>` | The last block is a tool call still running (or its arguments still streaming); the summary is the same one-line summary as its tool card. |
| `Thinking…` | The last block is reasoning still streaming, or the message has no blocks yet (between the turn's start and its first part). |
| `Writing…` | The last block is answer text still streaming. |

While a streaming assistant message has no blocks yet, its header's `●`
marker is the spinner too, so a slow first token still shows the turn is
alive before the working line's own elapsed clock is very interesting.

**Status bar.** One muted line under the header: the permission mode
(`mode <mode>`, from `SessionInfo.permissionMode`, colored per mode — see
[Permission modes](#permission-modes)), the workspace directory
(shortened, keeping the tail), the git branch (`GetVcsStatus`, refreshed
when a session opens and after a turn ends; omitted when unknown or the
directory is not a repository), a compact todo count (`Todos <completed>/
<total>`) shown only while the sidebar is hidden (the sidebar's own `Todos`
box already lists them), and `reconnecting` while the session event stream
is down. Segments with no data are omitted rather than shown empty; on a
narrow terminal the least essential segments (from the end) drop first, then
the whole line clips, so it always fits the terminal width. The header line
above it already carries agent, model, session, and server, so the status
bar does not repeat them.

Context-usage percent and a session token total are part of the Tier 1
design (latest assistant usage vs. the model's context limit; the sum of
recorded token usage) but are not on the `hya.v1` wire yet: `ModelSummary`
has no context-limit field, and `MessageInfo` carries no usage — the
`TokensRecorded` event is not mapped onto the `StreamEvent` stream either.
Both fields are always omitted here (the same "hide if unknown" rule the
design gives context percent); a later backend change can add them without
another TUI change once the fields exist.

**Todo panel.** The sidebar's `Todos` box is seeded from `GetSessionTodo`
when a session opens and kept current by the same debounced refresh that
re-reads messages and interactions after a durable stream frame (so a
`todo__update_status` or `todo__update_content` tool call's completion
refreshes it, typically within a few hundred ms — there is no `TodoUpdated`
stream frame yet; see the note above). Each item is one line, a status
glyph and its text: pending `○` (muted), in progress `◐` (accent),
completed `✓` (green), blocked `✗` (muted — the `TodoStatus` enum has no
`cancelled` status, so `blocked` takes the glyph and color that status would
otherwise use). The box shows at most 6 items, then a `+N more` row, so a
long list cannot push the `Context` box below the visible area. `/todos`
still opens the full-panel view (same glyphs) for a longer list.

## Notices

**Compaction.** A `CompactionApplied` event renders as a muted transcript
divider, `── context compacted · <strategy> ──`, spliced in right after the
message that was newest in the transcript when it fired (or at the end if
that message is no longer in the rendered window). The event carries a
watermark sequence and the strategy that fired (`shake`, `remote`, `soft`,
`snap_compact`, `handoff`), not a message count, so the divider does not
report one. Only the engine's automatic mid-turn compaction strategies emit
this event; the manual `/compact` command (`CompactSession`) injects a
system message instead and does not produce a divider.

**Engine system messages.** A message with the system role (for example a
`TEAM QUIESCED …` coordination notice) renders as a muted notice line, not
an assistant header block — no `●`, no agent or model.

**Connection and version.** A lost stream connection shows `Stream
reconnecting: <error>` in the status line while it retries (also reflected
in the status bar's `reconnecting`); a version mismatch between this TUI and
the backend's bootstrap version appends `backend <version> ≠ tui <version>`
to the initial `Connected to hya …` status.

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
| Tool call | A card: `<icon> <tool>  <summary>` and the duration on the right, collapsed by default. See [Tool calls](#tool-calls). A `task` call is a subagent card; see [Subagents](#subagents). |
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

### Tool calls

Every tool call of an assistant message is a card. The header is one line:

```text
✓ read  src/main.rs · lines 1-40 of 212                          3ms
⠹ bash  cargo test -p hya-core
◌ bash  rm -rf target · awaiting approval
✗ read  missing.txt
  File not found: /work/missing.txt
```

- **State icon.** `○` pending (the model is still streaming the
  arguments), a spinner (`⠋⠙⠹…`, accent) while it runs, `◌` (warning color)
  while a permission request for this call waits (its interaction's
  `payload.callId` is the card's call id), `✓` (green) done, `✗` (error
  color) failed.
- **Tool name** in bold, then a **summary** (muted) that depends on the tool
  (below), clipped to the width, and the **duration** on the right once the
  call is done (`42ms`, `1.5s`, `12s`, `1m 5s`).
- A failed call adds its error message on the next line in the error color,
  collapsed or not.

**Expanding.** Cards are collapsed by default; expanded, the body shows under
the header beside a bar. Ctrl+G or `/tools` expands or collapses all of them
(`/tools on`, `/tools off`; with no argument it toggles) and forgets
per-card choices, like `/thinking`. A click on one card toggles just that
card; the input keeps the focus. The cards of a `!command` shell turn start
expanded, so you see the output you asked for. A body longer than 12 lines
keeps its first 5 and last 6 lines around a `… N lines hidden` row.

| Tool (canonical name) | Summary | Expanded body |
| --- | --- | --- |
| `bash` (hidden alias `shell`) | The command (first line), then `· exit N` for a non-zero exit and `· timed out` | `$ <command>`, the output (muted), the exit status (error color) |
| `read` | `<path> · lines A-B of N` (from the output's display metadata; before that, from `offset` / `limit`) | The text with line numbers |
| `edit` | `<path> · +A -D` | The diff: the output's `metadata.diff` (unified diff), else rows derived from the arguments (`edits[].oldText`/`newText`, `lines`; compat `oldString`/`newString`) |
| `write` | `<path> · N lines` | The content, every row an addition |
| `apply_patch` (alias `patch`) | The files, `· +A -D` | The patch envelope: file headers, `@@` hunks, `+`/`-`/context rows |
| `grep` | `"<pattern>" in <path> (<glob>) · N matches` | `file:line: text` per match |
| `glob`, `find` | `<pattern> in <path> · N files` | The paths |
| `ls` | `<path> · N entries` | The listing |
| `lsp` | `<operation> <file>:<line>:<character>` | The output |
| `todo__read`, `todo__update_status`, `todo__update_content` (and older `todo*`) | `N todos · D done` | The list, `☐` pending, `▸` in progress, `!` blocked, `✓` completed |
| `webfetch` (alias `fetch`) | The URL | The output |
| `websearch` (alias `search`) | `"<query>"` | The output |
| `skill` | The skill name | — |
| `ask_user` (alias `question`) | `<header>: <question>` of the first question | The answers |
| `task` | `<subagent_type> · <description>` | A subagent card (below) |
| anything else (MCP `server__tool`, plugin tools) | The arguments as compact JSON | The output text |

Diff rows are colored: `+` added (green), `-` removed (red), hunk and file
headers blue, context muted. While a call's arguments still stream (state
`PENDING`, `inputJson` not complete), the summary reads the main string field
(`command`, `path`, `pattern`, `url`, `query`, …) out of the partial JSON.

Cards appear and update as the stream frames arrive, before the projection
is re-read (see [Stream frames and the transcript](#stream-frames-and-the-transcript)).

### Subagents

A `task` call spawns a subagent in its own child session. Its card shows the
child's status and what it last did, and it always shows these lines:

```text
✓ task  general · survey the repo                                  7ms
│ ⠹ running  ↳ read notes.txt · lines 1-1 of 1
│ click to view · /open hysec_…
```

- **Link.** The card finds its member (`MemberInfo`) by the tool call's
  `callId` (`memberUpdated.callId`), else by the child session in the task
  output (`outputJson.metadata.sessionId`). Resident spawns record no call
  id, so the second rule is the one that usually applies.
- **Status.** A finished member status wins (`✓ done`, `✗ failed`,
  `! cancelled`). Otherwise the child session's `busy` flag says `running`
  (spinner) or `idle` (`✓`, its turn ended; a resident subagent waits for
  mail). Before anything is known it is `○ starting`. `✗ failed` also shows
  when the child's newest reply failed.
- **Waiting.** While the child session has a pending permission request
  (question), the status reads `◌ waiting for approval` (`◌ waiting for an
  answer`) in the warning color, and the parent view shows the ask as a
  prompt labelled with the subagent (see
  [Subagent asks](#subagent-asks)).
- **Latest activity** after `↳`: the member's finish `summary` when it has
  one, else the child's newest tool call (`<tool> <summary>`) or the first
  line of its newest text.
- **Source.** The TUI reads each child of the open session (its members and
  the children named by `task` outputs) with `GET /v1/sessions/{child}`
  (`busy`, `agent`) and `GET /v1/sessions/{child}/messages` (activity). It
  reads them after every projection read and `memberUpdated` frame, at most
  once per 1.5 s, and repeats every 1.5 s while a child is busy or this
  client's turn runs. It does not subscribe to child streams. The same round
  re-reads the session list, so the sidebar's nesting and `· running` stay
  current, and the pending interactions (`GET /v1/interactions`), so a
  subagent's ask reaches the parent's prompt within one round.

**Child view.** A click on the task card, `/open <child session id>`, or
`/open <number>` of its sidebar row opens the child session read-only: a
`Viewing subagent <agent> · Esc returns · read-only` banner sits above its
transcript, and the input's placeholder and the footer say so. Enter on a
prompt or `!command` keeps the text and shows
`Read-only: this is a subagent's session · Esc returns to the parent`;
slash commands still run. Esc, when no list is open, opens the parent session
again (status `Back to the parent session`); the text you typed stays, and a
second Esc clears it. Opening another session this way resets the parent's
overlay and prompt queue like any session switch; the parent's turn keeps
running on the server, and its transcript is re-read on return.

## Streaming, queued prompts, and turn status

The assistant reply appears chunk by chunk while the model streams it. When
the reply is complete, the transcript shows the server's stored copy of it;
the text does not repeat or flicker when that happens.

You can type the next prompt while a turn is running. Press Enter and the
prompt appears dimmed at the end of the transcript, tagged `queued`. The working line counts the waiting prompts
(`Queued 1`). When the running turn ends, the frontend sends
the oldest queued prompt; several queued prompts go one per turn, in the
order you typed them. The server has no prompt queue of its own. It rejects
a prompt with `409 session_busy` while a turn runs, and it releases the
session shortly after the reply finishes. So the frontend retries a busy
prompt a few times with a short backoff (about 100 ms growing to 1 s). If the
session is still busy after that (for example, another client started a
turn), the prompt stays queued and is sent after the next turn end seen on the
stream. Opening another session drops the queued prompts of the previous one.
A queued prompt is still sent after a cancelled or failed turn.

The status line above the input shows the turn state. While the
[working line](#working-indicator-status-bar-and-todo-panel) shows a
running turn, the progress texts that repeat it (`Sending prompt…`,
`Running · <turn id>…`, `Running shell · …`, `Queued · N waiting`) are left
out of the status line, which stays empty until another message (a command
result, an error, `Ready`) arrives:

| Status | Meaning |
| --- | --- |
| `Sending prompt…` | The prompt is being admitted (`CreateTurn` in flight). |
| `Session busy · retrying (N)` | The server answered `409 session_busy`; the prompt is retried. |
| `Running · <turn id>[ · N queued]` | The turn runs; `N` prompts wait. Hidden while the working line shows. |
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

**Esc.** Esc closes the file list if it is open. With a permission or
question prompt shown and an empty input, it then denies the permission or
rejects the question (see
[Permission and question prompts](#permission-and-question-prompts)). In a
subagent's read-only view it then returns to the parent session (see
[Subagents](#subagents)).
Otherwise, while a turn
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
runs, so a permission prompt may appear; press `1` to run the command once
(or `/approve <id>`).
`CreateTurn` returns only when the command has finished; meanwhile the status
reads `Running shell · <command>`.

The backend records the turn as two messages: a user message with the fixed
text `The following tool was executed by the user`, and an assistant message
with one `bash` tool call. The transcript shows the user message as
`!<command>` and the tool call as a `bash` card (see
[Tool calls](#tool-calls)) that starts expanded:

```text
┃ !echo hello

● build · openai/gpt-5
◌ bash  echo hello · awaiting approval
```

and, once approved and finished:

```text
✓ bash  echo hello                                               4ms
│ $ echo hello
│ hello
```

The command comes from this TUI's own shell turns (before the part carries
its input), or from the tool call's `inputJson` (`{"command": …}`); the
output is the tool call's `outputJson`. Esc cancels a running shell command;
the turn then reads `Cancelled · Ready`.

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

## Permission and question prompts

The backend asks before some tool calls run (under the default permission
model: `bash`, `edit`, `write`, network reads, MCP and plugin tools; see
[Configuration — Permissions](configuration.md#permissions)), and the
`ask_user` tool asks you questions. The TUI shows each of these pending
interactions as a prompt docked above the status line, so you can answer
without typing its id. The agent's turn waits until you answer.

### Permission prompt

```text
┌─Permission · 1 of 2──────────────────────────────────────────┐
│edit  src/main.rs · +1 -1                                     │
│asked by build                                                │
││ - let x = 1;                                                │
││ + let x = 2;                                                │
│▸ 1  Allow once                                               │
│  2  Always allow  tool: edit                                 │
│  3  Deny                                                     │
│1-3 or ↑↓ Enter · Esc denies · perm_… · mode manual           │
└──────────────────────────────────────────────────────────────┘
```

- **Title row.** The waiting call as its tool card would summarize it
  (`<tool>  <summary>`); an ask that is not tied to a tool call (for example
  an external directory) shows the interaction title, `<action> <resource>`.
- **Who asks.** `asked by <agent>` for the open session, `asked by subagent
  <agent> · <task description>` for a subagent's session.
- **Details**, beside a bar, rendered like the tool card body from the
  call's arguments (`payload.input`): `bash` the command (`$ …`); `edit`,
  `write`, `apply_patch` the diff (added rows green, removed rows red);
  `read`, `webfetch` and other path or URL tools the path or URL; anything
  else the compact JSON arguments; without a call, the resource. At most 8
  lines (the first 3 and last 4 around `… N lines hidden`).
- **Options.**

| Key | Option | Sends |
| --- | --- | --- |
| `1` | Allow once: this call runs. | `{permission: {allowed: true, persist: false}}` |
| `2` | Always allow: this call runs, and the backend stops asking for what the muted text names (`payload.always`, shown as `<action>: <patterns>`; the resource when the backend sends no patterns). | `{permission: {allowed: true, persist: true}}` |
| `3` | Deny: the call fails with a permission error (its card shows `✗`), and the model continues with that result. | `{permission: {allowed: false, persist: false}}` |

  An Always allow grant lives in the running backend process: it applies to
  every session of that backend (and survives a permission mode switch) until
  the backend restarts. For native tools it covers the exact subject (the
  same command, the same path); see
  [Tools and permissions](architecture/tools-and-permissions.md).

### Question prompt

```text
┌─Question─────────────────────────────────────────────────────┐
│Color: Which color do you want?                               │
│asked by build                                                │
│▸ 1  red                                                      │
│  2  blue                                                     │
│  3  Other…  type the answer in the input, Enter sends        │
│  4  Reject                                                   │
│1-4 or ↑↓ Enter · type an answer + Enter · Esc rejects · q_…  │
└──────────────────────────────────────────────────────────────┘
```

The first row is `<header>: <question>`. Each option sends
`{question: {answer: "<label>"}}`. For a free-text answer, type it into the
input and press Enter: it sends `{question: {answer: "<text>"}}` instead of a
prompt (a `/command` still runs as a command). Choosing `Other…` only points
you at the input. `Reject` (or Esc) sends `{question: {rejected: true}}`;
`ask_user` then reports the question as unanswered. Only the first question
of a multi-question `ask_user` call is shown (the backend answers one per
interaction).

### Keys

A prompt takes keys only while the input is empty, so text you are typing
can never answer one by accident: with text in the input, digits, Enter, and
Esc edit or send the input as usual, and the hint row reads `Clear the input
to answer with 1-3 · or /approve <id>` (a question's reads `Enter sends the
input as the answer`). The text you typed stays in the input while a prompt
is shown and after you answer it.

Key order, first match wins:

1. The permission mode picker, while open, takes every key but Ctrl+C; the
   one-line yolo confirmation takes Enter, Esc, and Shift+Tab (see
   [Permission modes](#permission-modes)), so they never answer the prompt.
2. An open list (the `/` command menu or the `@file` list) takes Up/Down,
   Tab, Shift+Tab (highlight up), Enter, and Esc.
3. The prompt, with an empty input: `1`–`9` choose that option at once;
   Up/Down move the highlight (`▸`, accent color); Enter chooses the
   highlighted option; Esc declines. With text in the input, a question takes
   Enter as its answer.
4. The composer: history, sending, Esc's other meanings (return from a
   subagent view, cancel the turn, clear the input), and Shift+Tab (switch
   the permission mode, also while a prompt is shown).

**Esc declines, it never approves.** On a permission prompt Esc is Deny
(`allowed: false`, not saved); on a question it is Reject. Denying is the
safe default: nothing runs that you did not allow, and the model sees the
refusal and can continue or ask differently. There is no "answer later"
state; to leave a prompt waiting, type in the input (the prompt stays and
ignores keys) or open another view. A click on an option chooses it too.

### Several asks

Asks are shown one at a time, oldest first; the box title counts them
(`Permission · 1 of 2`). Answering one shows the next. The session's own
tool calls ask one after another; several asks wait at once when subagents
ask too.

An answer hides the prompt at once. The ask also closes when it is resolved
elsewhere: another client answered it, or a switch of the session tree to
the `yolo` permission mode allowed it (an `interactionResolved` frame, or the
next listing). If the ask was already answered, the status line reads
`Already answered elsewhere · <title>`; if the request fails, the prompt
comes back with `Answer failed: …`.

### Subagent asks

A subagent runs in a child session and asks in its own name. The open
session's prompt queue holds the asks of the open session and of every
session below it (children by `SessionInfo.parent`, and the open session's
members and `task` outputs before the session list knows them), so a
subagent's ask appears in the parent view, labelled `asked by subagent
<agent> · <task>`. The subagent's `task` card shows `◌ waiting for
approval`, and its sidebar row `· ◌ waiting`. Opening the subagent's
read-only view shows the same prompt there (only that subtree's asks); you
can answer in either view. Asks of unrelated sessions stay in the
[pending block](#layout).

The keyboard commands keep working as a fallback: `/approve <id>`,
`/deny <id>`, `/answer <id> <text>`, and `/interactions` (the prompt's hint
row shows the id).

## Permission modes

A permission mode decides how the open session tree (the session and its
subagents) answers permission checks: `manual` asks you (the
[permission prompt](#permission-prompt)), `yolo` allows every tool call
without asking (including calls a rule denies), and a bundle mode lets an
installed bundle's approver answer first and asks you only for what it
leaves open. The mode lives on the backend, on the root session; see
[Configuration — Session permission modes](configuration.md#session-permission-modes)
for the semantics. The TUI switches it without a restart, shows it in the
status bar, and notes every switch in the transcript.

### Switching

- **Shift+Tab** switches to the next mode: `manual` → `yolo` → the bundle
  modes in the backend's listing order → `manual`. The listing is read with
  the other catalogs at start and on `/refresh`; without it (an older
  backend) the cycle is `manual` ↔ `yolo`. Shift+Tab also works while a
  permission prompt is shown. In an open list (the `/` command menu, the
  `@file` list, the picker) it moves the highlight up instead and the mode
  does not change.
- **`/permissions`** opens a picker with every mode from
  `GET /v1/permission-modes`: its title, `[source]` (`builtin`, or the id
  of the bundle that declares it), and description; `●` marks the mode in
  effect, which is also highlighted. Type to filter (every word must match
  the title, id, source, or description), Up/Down (or Shift+Tab/Tab) move,
  Enter switches, Esc closes; a click on a row switches too. While the
  picker is open the input does not take keys; closing it gives the input
  the focus back.
- **`/permissions <mode>`** switches directly (Tab completes the mode ids),
  for example `/permissions yolo` or `/permissions acme/approver/careful`.

```text
┌─Permission mode──────────────────────────────────────────────────────┐
│ Filter ▏  3 of 3                                                     │
│ ▸ ● Manual     [builtin]       Ask the user before actions that need │
│     Yolo       [builtin]       Allow every action without asking, in │
│     Echo only  [e2e/approver]  Approve echo commands; ask for the re │
│ ↑↓ select · Enter chooses · Esc closes · type to filter              │
└──────────────────────────────────────────────────────────────────────┘
```

**Confirming yolo.** The first switch to `yolo` in a TUI process shows one
line above the status line and waits:

```text
⚠ Enable yolo? Every tool call runs without asking · Enter confirms · Esc cancels
```

Enter switches; Esc keeps the current mode (`Permission mode unchanged ·
manual`); Shift+Tab skips `yolo` and goes on to the next mode of the cycle
(with only the built-ins, that is where you started, so nothing changes);
any other key cancels and then does what it normally does, so you cannot
type into `yolo` by accident. This line takes Enter and Esc before a shown
permission prompt does. After one confirmed switch, later switches to `yolo`
in the same process do not ask again. Shift+Tab never lands in `yolo`
without this confirmation.

**Pending asks.** Switching to `yolo` makes the backend allow (once) every
permission ask of the tree that is still waiting, so the prompt closes and
the tool runs: the TUI re-reads the pending interactions right after the
switch instead of waiting for the `interactionResolved` frames. The switch
applies from the next permission check, including in a turn that is
already running.

**No session yet.** Before any session exists (a fresh directory), the
choice is remembered — the status bar shows it and the status line reads
`Permission mode → <mode> · applies when the session is created` — and it
is sent right after the next session is created (the first prompt, `/new`,
or a command that creates one), before the prompt is admitted. Opening an
existing session instead shows that session's own mode.

### Display

| Mode | Status bar | Color |
| --- | --- | --- |
| `manual` | `mode manual` | normal text (`fg`) |
| `yolo` | `mode ⚠ yolo` | `error` (`#f07878`) |
| bundle mode | `mode <title>` (the listing's title, else the id) | `accent` (`#73c8e8`) |

The `mode` word and the rest of the status bar stay muted. Every switch —
from this TUI or another client (a `sessionUpdated` frame with
`permissionMode`) — adds one muted notice line to the transcript,
`Permission mode → yolo` (a bundle mode: `Permission mode → <title>
(<id>)`), and the status line confirms it (`Permission mode → ⚠ yolo ·
Shift+Tab cycles · /permissions lists`). A permission prompt's hint row ends
with the mode (`… · perm_… · mode manual`). An unknown or unavailable mode
leaves the mode unchanged and shows `Permission mode failed: …
invalid_argument: …`.

### Bundle modes

A bundle declares modes with `permission_modes:` and answers them with a
`permission.approve` hook (see
[Agent bundle authoring — Permission modes](agent-bundle-authoring.md#permission-modes-permission_modes)).
Installed (for example `hya bundle install --project -y approver.hyabundle`,
or a source directory under `.hya/bundles/<dir>/` where `hya serve` runs),
its modes appear in the picker as `<title> [<bundle id>]` and in the
Shift+Tab cycle after `yolo`. With one active, the approver decides first;
when it defers, the TUI shows the usual permission prompt. Worked example:
a bundle `e2e/approver` whose mode `echo-only` allows `echo …` commands —
`/permissions`, type `echo`, Enter: the status bar reads `mode Echo only`,
a model's `echo hi` call runs without a prompt, and its `ls` call asks.

## Pickers

`/model`, `/agent`, and `/sessions` (with no argument) open the same
reusable modal picker `/permissions` uses (see
[Permission modes — Switching](#switching) for the shared filter/move/select
keys). Rows are loaded from the catalog already held by the TUI (`refresh()`
at start and `/refresh`/Ctrl+R), so a picker opens with no loading state.

- **`/model`** lists every model from `GET /v1/models`, `[tag]`ged with its
  provider id and, when the route advertises one, its context window
  (`128k ctx`); `●` marks the open session's model. Enter sends
  `UpdateSession {model}` and shows `Model → <provider>/<model>`.
  `/model <provider/model>` still switches directly, with Tab completion.
- **`/agent`** lists visible (non-`hidden`) agents from `GET /v1/agents`,
  tagged with the agent's default `provider/model` and its one-line
  description; `●` marks the open session's agent. Enter sends
  `UpdateSession {agent}` and shows `Agent → <name>`. `/agent <name>` still
  switches directly.
- **No session yet.** Before any session exists, a `/model`/`/agent` choice
  (picker or direct form) is remembered — the status line reads
  `Model → <id> · applies when the session is created` (`Agent → …` for the
  agent) — and is used for the next `CreateSession` in place of the usual
  default, the same way a chosen [permission mode](#permission-modes)
  applies once the session exists.
- **`/sessions`** opens a picker with a `New session` row first, then every
  session as a tree (top-level sessions, subagent sessions nested under
  their parent and `[subagent]` tagged — see [Subagents](#subagents)),
  showing the agent, model, and a relative update time (`3m`, `2h`) in the
  detail column, and `● running` while busy. `●` marks the open session.
  Enter on the `New session` row runs `/new`; Enter on any other row opens
  it.

```text
┌─Sessions──────────────────────────────────────────────────────────────┐
│ Filter ▏  3 of 3                                                      │
│ ▸   New session          [new]       Create a session with the curr… │
│   ● Fix the flaky test              build · fake/model · 3m           │
│       ↳ Explore the auth code [subagent]  explore · fake/model · 1m  │
│ Enter opens · F2 renames · Ctrl+D deletes · Esc closes · type to fil… │
└──────────────────────────────────────────────────────────────────────┘
```

### Row actions

The `/sessions` picker's highlighted row also takes two keys the plain
filter never sees (never Ctrl+R, which means refresh):

- **F2** renames it: the picker switches to a one-line editable field seeded
  with the row's current label (`New title <text>▏`); type to edit, Enter
  sends `UpdateSession {title}` and shows `Renamed to <title>` (an empty
  title cancels with a status message), Esc returns to the list without
  changing anything. A rename reopens the picker so browsing continues, its
  row already showing the new title.
- **Ctrl+D** deletes it: the picker switches to a one-line confirmation
  (`Delete "<title>"? Enter confirms · Esc cancels`); Enter sends
  `DeleteSession` and shows `Deleted session <id>`, Esc returns to the list
  with nothing changed. Deleting the open session opens the next top-level
  session (or shows no session, if none is left); the confirmation applies
  the same way whether or not the row is the open session, so the open
  session is never deleted without it.

`state/picker.ts`'s `PickerAction` (`{id, key, ctrl?, label, prompt: "value"
| "confirm", confirmText?}`) and the `"rename"`/`"confirm"` picker modes are
a small, backward-compatible extension of the picker used by `/permissions`:
a picker with no `actions` behaves exactly as before. See
[Code layout — The picker](#code-layout) for the API.

### Session titles

The header (`hya · <title or id> · <agent> <provider/model> · <server>`),
the sidebar's `Sessions` box, and the `/sessions` picker all show the
session's `title` when the backend has set one (`/rename`, the picker's F2,
or the backend's own auto-generated title once it lands), falling back to
the raw id. A `sessionUpdated {title}` frame (see
[Stream frames and the transcript](#stream-frames-and-the-transcript))
updates all three live, with no extra refresh — including a title set by
another client or generated by the backend after the first turn.

## Interface definitions

The frontend uses the existing HTTP/JSON+SSE transport. Every request carries
`x-hya-directory: <absolute --dir path>`; JSON uses protojson lower camel case,
string encoded 64-bit values, and the error envelope documented in the
[protocol guide](protocol/README.md). These are the first-class calls:

| Method and route | Request | Response read by the TUI |
| --- | --- | --- |
| `GET /v1/bootstrap` | No body | `Bootstrap` (`location`, `agents`, `models`, `interactions`) |
| `GET /v1/sessions` | No body | `ListSessionsResponse.sessions: SessionInfo[]` (every session of the directory, subagent sessions included; `parent` nests them in the sidebar and the `/sessions` picker, `busy` marks `· running`, `timeUpdated` feeds the picker's relative time). Re-read with each child-session round (see [Subagents](#subagents)). |
| `POST /v1/sessions` | `{agent: string, model: string, workdir: string}` | `CreateSessionResponse.session: SessionInfo` |
| `GET /v1/sessions/{id}` | No body | `SessionInfo` (including `permissionMode`, read by `/status`; `parent`, which makes the view read-only; `members: MemberInfo[]`, the subagent rows the task cards link to). For a child session: `busy` and `agent` for its task card. |
| `PATCH /v1/sessions/{id}` | `{title?: string, model?: string, agent?: string, permissionMode?: string}` (`UpdateSession`; `/model`, `/agent`, `/rename`, the `/sessions` picker's F2, and a permission mode switch each send one field; `permissionMode` is `manual`, `yolo`, or `<bundle-id>/<mode-id>`) | `SessionInfo`; after a switch its `permissionMode` is the mode shown. An unknown or unavailable mode fails with `invalid_argument`. |
| `DELETE /v1/sessions/{id}` | No body (`DeleteSession`; the `/sessions` picker's Ctrl+D, confirmed first) | Empty response; the TUI re-reads the session list and, if the deleted session was open, opens the next top-level one. |
| `GET /v1/agents` | No body (`ListAgents`; read with the catalogs and by `/agent`) | `ListAgentsResponse.agents: AgentSummary[]` (`name`, `model`, `description`, `hidden`); the `/agent` picker drops `hidden` rows. |
| `GET /v1/permission-modes` | No body (`ListPermissionModes`; read with the catalogs and by `/permissions`; a `404` from an older backend counts as an empty list) | `ListPermissionModesResponse.modes: [{id, title, description, source}]` — built-ins first; `source` is `builtin` or the bundle id. Feeds the Shift+Tab cycle, the picker rows, and bundle mode titles. |
| `GET /v1/sessions/{id}/messages` | No body | `ListMessagesResponse.messages: MessageInfo[]`; tool cards read `parts[].toolCall` (`ToolCallPart {callId, tool, state, inputJson, outputJson, durationMs, errorCode, errorMessage}`). For a child session: its latest activity. |
| `POST /v1/sessions/{id}/compact` | `{}` (`CompactSession`) | `CompactSessionResponse {compactedUntilSeq, strategy}` for `/compact` |
| `POST /v1/sessions/{id}/summarize` | No body (`SummarizeSession`) | `SummarizeSessionResponse {summaryMessage}` for `/summarize` |
| `GET /v1/sessions/{id}/todo` | No body (`GetSessionTodo`) | `TodoList.items: TodoItem[]` for `/todos` and the sidebar's live `Todos` box (seeded on session open, kept current by the same debounced refresh as messages and interactions). |
| `GET /v1/vcs?directory=<--dir>` | No body (`GetVcsStatus`) | `VcsStatus.branch` for the status bar's git branch; read when a session opens and after a turn ends. Never errors on a non-repository directory (`branch` comes back empty, so the segment is omitted). |
| `POST /v1/sessions/{id}/turns` | `{prompt: {text: string}}` | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{command: {command: string, arguments: string}}` for other slash commands | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{shell: {command: string, agent: string, model?: {providerId: string, modelId: string}}}` for `!command` (the session's agent and model) | `CreateTurnResponse.turn: TurnInfo` once the command has finished; `id` is the shell turn's assistant message. |
| `POST /v1/sessions/{id}/turns/{turn}/cancel` | `{}` | `TurnInfo`. Esc and `/cancel` send the admitted turn id (the user message id). The server cancels whatever runs in the session, so a shell turn whose id is not known yet is sent as `current`. |
| `GET /v1/fs/find?pattern=**/*<text>*&limit=50` | No body (`FindFiles`, scoped by `x-hya-directory`) | `FindFilesResponse.paths: string[]` (relative paths) for `@file` suggestions. |
| `GET /v1/sessions/{id}` | No body | `SessionInfo.lastSeq` when a session is opened (the stream's first `sinceSeq`). |
| `GET /v1/sessions/{id}/events/stream?sinceSeq=N` | SSE | `StreamFrame` with `event` or `resync`; `N` is the last applied durable seq. |
| `GET /v1/sessions/{id}/events?sinceSeq=N&limit=500` | No body | `ListEventsResponse.events` / `nextSeq`, paged, to fill the gap after each stream (re)connect and `resync`. |
| `GET /v1/interactions` | No body (every type, every session; read at start, on refresh, after interaction frames, and in each child-session round) | `ListInteractionsResponse.interactions: Interaction[]`, oldest first. The TUI reads `id`, `session` (the asking session, a subagent's child session included), `type` (`INTERACTION_TYPE_PERMISSION` / `_QUESTION`), `title`, `detail` (a question's header), `options` (a question's option labels), and a permission's `payload`: `action`, `resource`, `always` (what Always allow covers), `callId` (marks the waiting tool card, `◌ … · awaiting approval`), `tool` and `input` (the prompt's details). A listed question has no options or header; the TUI keeps those from its live `questionRequested` frame, else reads them from the waiting `ask_user` call in the transcript. |
| `POST /v1/interactions/{id}/respond` | Prompt: `{permission: {allowed: boolean, persist: boolean}}`, `{question: {answer: string}}`, or `{question: {rejected: true}}`. `/approve`, `/deny`: `persist: false`. | `RespondInteractionResponse.applied` (`false`: already resolved elsewhere) |
| `GET /v1/models` | No body | `ListModelsResponse.models: ModelSummary[]` (`id`, `providerId`, `modelId`, `displayName`, `contextLimit`); the `/model` picker tags rows by `providerId`. |
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
| Chat in a subagent's session | `Read-only subagent view · Esc returns to the parent · click a task card or /open <n> to switch` |
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
| `partStarted {message, part, kind: "tool_call", tool, callId}` | durable | Overlay tool part in state `PENDING` (a `○` card). |
| `partAppended {message, part, textDelta}` | live (assistant text) or durable (reasoning, tool arguments, user text) | Appends `textDelta` to the part (for a tool part, to its argument JSON so far). No projection re-read. |
| `toolStateChanged {message, part, callId, state, tool, inputJson, outputJson, durationMs, errorCode, errorMessage}` | durable | Sets the tool part's state and every field the frame carries, keeping the others (`inputJson` replaces the appended fragments). An empty `callId` is a direct part overwrite; the part keeps its call id. A part the overlay never saw is started. Merged with the projection by part id: the overlay's part is shown only while its state is further along (`PENDING` < `RUNNING` < `OK`/`ERROR`). |
| `memberUpdated {member, child, agent, description, status, summary, callId, depth}` | durable (parent session) | Folded into the open session's member rows by `member` (partial frames keep known fields); triggers a child-session round. |
| `partReplaced {message, part, text}` | live (plugin rewrite) or durable (end of round) | Sets the part's whole text, replacing the live deltas. |
| `partCompleted {message, part}` | live or durable | No overlay change; a durable one triggers a projection re-read. |
| `errorReported {message, code, errorMessage}` | durable | Stored as the message's error. Shown in the transcript and, at turn end, in the status line. |
| `messageFinished {message, finish, cause}` | durable | The turn ends at the first assistant `messageFinished` after the turn's user message whose `finish` is not `FINISH_REASON_TOOL_CALLS`. Then the projection is re-read. |
| `permissionRequested {interaction}`, `questionRequested {interaction}` | live | The ask is added to the pending list at once (a prompt appears); its options and header are remembered by id; then the listing is re-read. Only the open session's own asks arrive here; subagent asks come from the listing. |
| `interactionResolved {request}` | live | The ask is removed at once (its prompt closes); then the listing is re-read. |
| `sessionUpdated {permissionMode}` | durable (root session) | The tree's mode changed (this TUI's switch echoed, or another client's): the open session's `permissionMode` is updated, and a `Permission mode → …` notice is added unless the transcript already announced that mode. |
| `sessionUpdated {title, agent, model}` | durable | Patches the session's row (and, if it is the open one, the header and sidebar) at once — a `/rename`/`/model`/`/agent` from another client, or the backend's auto-generated title (see [Session titles](#session-titles)) — instead of waiting for the next catalog refresh. |
| `compactionApplied {untilSeq, strategy}` | durable | Appended to `state.dividers`, spliced into the transcript right after the message that was newest at the time (see [Notices](#notices)); replayed by `ListEvents` like any other durable event, so reopening a session that had one restores its divider. |
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
| `src/state/store.ts` | `createAppStore()`: the single store. It holds the server projection (sessions, messages, interactions, models, agents, providers, workflows, saved key names, backend commands, todos, stream cursor, the open session's subagent members, what was last read about each child session), the published streaming overlay, the prompt queue, the turn state (`running`, `turnId`), and UI state (view, status, key-entry provider and mask, sidebar mode, terminal columns, the reasoning switch and per-part toggles, the tool-card switch and per-card toggles, the highlighted prompt option (`promptSelection`, by ask id), whether the input holds text (`draft`), the jump-to-bottom tick, the `/status` text, the backend version from bootstrap, the `/name args` display text of command turns by user message id). Each field is a Solid signal, and only the store's mutation methods change it. |
| `src/state/overlay.ts` | `TranscriptOverlay`: the pure fold of stream frames by message and part id (seq filter, live/durable handover, `resync` handling, turn-end lookup). `mergeTranscript()` merges it over the projection. |
| `src/state/messages.ts` | The transcript view model: `transcriptViews()` (projection + overlay + waiting queued prompts), `messageView()` (role, agent/model, typed blocks, finish notice; cached per message object), `finishNotice()`, `reasoningLabel()`, `reasoningExpanded()`, `toolExpanded()`. |
| `src/state/tools.ts` | The tool-card view model: `toolCard()` (status, per-tool summary, body lines with tones, duration, error, task info), `toolStatus()`, `formatDuration()`, `clipLines()`, `diffLines()`, `partialField()`. |
| `src/state/modes.ts` | Permission modes: `modeCycle()` (Shift+Tab order), `nextMode()`, `requestMode()` and `confirmKey()` (the yolo confirmation state machine), `modeDisplay()` (status bar text and tone), `modeNotice()`, `modeRows()` (picker rows), `effectiveMode()`, `isShiftTab()`. |
| `src/state/picker.ts` | The reusable modal picker's pure state (API below): `createPicker()`, `pickerMatches()`, `pickerRows()`, `pickerKey()`, `pickerWindow()`, and the `PickerRow` / `PickerAction` / `PickerSpec` / `ActivePicker` types; `"rename"`/`"confirm"` row-action modes (F2/Ctrl+D on `/sessions`, [Pickers — Row actions](#row-actions)). |
| `src/state/catalog.ts` | `/model`/`/agent`/`/sessions` picker row builders: `modelRows()` (tagged by provider), `agentRows()` (visible agents, tagged by default model), `sessionRows()` (the `New session` row + `sessionTree()`, relative time), `relativeTime()`. |
| `src/app/modes.ts` | `createModeSwitcher()`: `cycle()` (Shift+Tab), `request(mode)`, `key()` (the confirmation's keys), `applyPending()` (a mode chosen before any session, sent after `CreateSession`); sends `UpdateSession {permissionMode}`, re-lists interactions, reports in the status line. |
| `src/state/prompts.ts` | Permission and question prompts: `promptQueue()` (asks of the open session's tree), `treeSessionIds()`, `promptView()` (headline, asker, details from `toolCard()`, options), `currentPrompt()`, `promptKey()` (option keys), `respondBody()`, `mergeInteractions()` (listing + live frames + answered ids), `waitingKind()`. |
| `src/app/prompts.ts` | `answerPrompt()`: send a choice's `RespondInteraction`, hide the ask, report the outcome in the status line. |
| `src/state/members.ts` | Subagents: `foldMember()`, `taskLink()` (card → member and child session), `childStatus()`, `childActivity()`, `childSessionIds()`. |
| `src/state/layout.ts` | Sidebar rules: `layoutBreakpoints`, `sidebarVisible()`, `toggledSidebar()`, `sidebarWidth()`, and `parseSwitch()` for `on`/`off` arguments. |
| `src/state/scroll.ts` | `ScrollFollow` (the "new messages below" hint), `atBottom()`, `pageStep()`. |
| `src/state/format.ts` | Pure text for the header, sidebar (session list with `sessionTree()` nesting, context box), pending lines, and the non-chat views. |
| `src/app/controller.ts` | `createController()`: refreshes, the session SSE loop (subscribe, `ListEvents` gap-fill, `resync`), batched overlay flushes, the debounced projection re-read (`app/debounce.ts`), child-session rounds for subagent cards, `returnToParent()`, session creation, prompt submission (refused in a subagent's read-only view), command dispatch, and concealed key entry. It writes results into the store. |
| `src/app/turns.ts` | `createTurnRunner()`: the client-side prompt queue, `409 session_busy` retry, and turn-end detection and status text. |
| `src/app/App.tsx`, `src/app/run.tsx`, `src/app/context.ts` | Root layout (main column + sidebar), renderer startup, and the `AppContext` (store, controller, server URL, and `ui` handles such as the transcript's scroll actions) that components read with `useApp()`. |
| `src/components/` | `Header`, `MainPanel` (transcript or view panel), `Transcript` (scrollbox, follow/hint), `MessageView` (`MessageItem`, user/assistant messages, blocks, reasoning, tool cards and `task` subagent cards, `KeyedFor`), `Spinner` (the shared spinner clock), `Markdown` (the `<markdown>` wrapper, `SyntaxStyle`, code-block boxes), `Panel`, `PendingBlock` (other sessions' asks), `PromptDock` (the permission / question prompt), `ModeConfirm` (the one-line yolo confirmation), `Picker` (the modal picker), `Sidebar`, `StatusLine`, `Composer` (the `<textarea>` editor, its height, history, Esc / Ctrl+C / Ctrl+D, the shell-mode border, the `@file` list, the `/` command menu, Tab completion, key actions, concealed key entry), `Footer`. |
| `src/composer/` | Pure composer logic: `history.ts` (`InputHistory`), `quit.ts` (`createQuitGuard`, the Ctrl+C double press), `escape.ts` (`escapeAction`), `shell.ts` (`shellCommand`, `isShellInput`), `mention.ts` (`mentionAt`, `insertMention`, `findPattern`, `rankPaths`). |
| `src/commands/` | The slash-command registry (`registry.ts`), the built-in commands (`native.ts`), the `/help` text (`help.ts`), and the command menu's merge/fuzzy-filter/argument-hint logic (`menu.ts`: `mergeCommandEntries`, `filterCommands`, `requiresArgument`). |
| `src/keys/bindings.ts` | The global key binding table (`keyBindings`, including `cycleMode` on Shift+Tab / CSI Z) and the textarea overrides (`composerKeyBindings`: Enter submits; Ctrl+J, Shift+Enter, Alt+Enter insert a newline; Home/End). |
| `src/completion.ts`, `src/instructions.ts`, `src/api.ts`, `src/theme.ts` | Tab completion and `SecretEntry`, footer instructions, the OpenAPI operation catalog, and the palette (`colors`, `syntaxColors`, and `syntaxStyles`, the Markdown/tree-sitter scope styles). |

The Solid transform has two parts. `bunfig.toml` preloads
`@opentui/solid/preload` for `bun test` and for `bun src/...` run inside the
package. `tsconfig.json` sets `"jsx": "preserve"` and
`"jsxImportSource": "@opentui/solid"`. Bun reads `bunfig.toml` only from the
directory it runs in, so `src/main.ts` imports the preload itself. It then
loads `.tsx` modules and `solid-js` with a dynamic `import()`. Keep static
imports in `main.ts` free of Solid code. Without the preload, Bun resolves
`solid-js` to its non-reactive server build.

**The picker.** `components/Picker.tsx` is a reusable modal list for
choosing one value (`/permissions`, `/model`, `/agent`, and `/sessions` all
use it). Open one from a command handler with `actions.openPicker(spec)`
(or `controller.openPicker`):

```ts
interface PickerRow {
  id: string          // value handed to onSelect (a mode id, model id, session id, …)
  label: string       // main text
  detail?: string     // muted text after the tag (a description)
  tag?: string        // shown as [tag] (a source, a provider, a kind)
  current?: boolean   // the value in effect: marked ●, highlighted when the picker opens
}
interface PickerAction {
  id: string           // outcome id passed to onAction, e.g. "rename", "delete"
  key: string           // OpenTUI key name, e.g. "f2", "d" — never a plain printable character
  ctrl?: boolean
  label: string         // hint text, e.g. "F2 rename"
  prompt: "value" | "confirm"   // "value" edits the row's label inline; "confirm" shows a yes/no line
  confirmText?: string  // "confirm" only; "{label}" is replaced by the row's label
}
interface PickerSpec {
  title: string       // box title
  rows: PickerRow[]
  hint?: string       // bottom row; default "↑↓ select · Enter chooses · Esc closes · type to filter"
  actions?: PickerAction[]   // row actions on the highlighted row (S9: /sessions F2/Ctrl+D)
  onSelect(row: PickerRow): void | Promise<void>   // runs after the picker closed; a throw shows "Error: …"
  onAction?(id: string, row: PickerRow, value?: string): void | Promise<void>   // after a row action committed
}

actions.openPicker({
  title: "Permission mode",
  rows: modeRows(modes, effectiveMode(store.state)),
  onSelect: (row) => actions.requestPermissionMode(row.id),
})
```

While a picker is open (`store.state.picker`), the composer's editor is
unfocused and its key handler sends every key but Ctrl+C to
`controller.pickerKey()`, which applies `pickerKey(state, key)` from
`state/picker.ts`: printable characters extend the filter (Backspace
shortens it, Ctrl+U clears it; an empty filter highlights the current row
again), Up/Down and Shift+Tab/Tab move with wrap-around, Enter selects, Esc
closes. At most `pickerMaxRows` (10) rows show; the window follows the
highlight. A click on a row selects it (`controller.choosePickerRow`).
Ctrl+C closes the picker and keeps its quit meaning. Selecting or closing
returns the focus to the input.

A key matching one of `spec.actions` switches the picker into `"rename"`
(`prompt: "value"`: an editable line seeded with the row's label; Enter
commits, Esc returns to the list) or `"confirm"` (`prompt: "confirm"`: a
one-line yes/no; Enter commits, Esc returns to the list) mode instead of
extending the filter. Committing (`controller.pickerKey()` sees a `"commit"`
outcome) closes the picker and runs `spec.onAction(id, row, value)`, the
same way selecting runs `onSelect`; the handler can call
`actions.openPicker` again to keep browsing (`/sessions`' F2 does, so a
rename reopens the picker on the updated row).

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
(Ctrl+B, `/sidebar`), the prompt dock, and the pending block of another session's ask. `e2e/hya-tui-messages.spec.ts`
covers user and assistant styling, Markdown and code highlighting, reasoning
(Ctrl+O, `/thinking`, click), error, length, and cancel notices, and
scrolling (PgUp/PgDn, End, Ctrl+End, the wheel, the new-messages hint).
`e2e/hya-tui-streaming.spec.ts` uses the fake model to cover streaming text,
queued prompts, and the turn status line (`Ready`, provider errors).
`e2e/hya-tui-commands-menu.spec.ts` covers the `/` command menu (open,
fuzzy filter, sources, Up/Down, Tab, Esc, Enter's argument-hint rule), skill
commands (a fixture `SKILL.md` under `.hya/skills/<name>/`), `/compact`,
`/rename`, and `/status`. `e2e/hya-tui-tools.spec.ts` covers tool cards (read, bash, edit/write diff
colors, a failed call, the running spinner, Ctrl+G, `/tools`, a click) and a
`task` subagent card (child status and activity, sidebar nesting, the
read-only child view, Esc back, `/open`), also at about 80 columns.
`e2e/hya-tui-composer.spec.ts` covers the composer:
Ctrl+J / Alt+Enter newlines and box growth up to 8 rows, Shift+Enter in the
browser, bracketed paste, cursor editing, history, Esc (clear, and cancel of
a hanging fake-model turn), Ctrl+C once and twice, Ctrl+D, `/exit`,
`!echo hello` (the waiting card, `/approve`, then its output), and `@file`
suggestions at the default width and about 80 columns.
`e2e/hya-tui-prompts.spec.ts` covers the permission and question prompts
under the default permission model: a bash ask (`1`, arrows + Enter, typed
digits going to the input), Always allow (`2`, a second identical call runs
without asking), Deny (`3`) and Esc, an edit ask's diff, two queued asks
(`1 of 2`), `ask_user` options, a free-text answer, and Reject, a subagent's
ask in the parent view (its task card and sidebar row waiting), a
`!command` shell ask, and about 80 columns.
`e2e/hya-tui-permission-modes.spec.ts` covers permission modes: Shift+Tab
through xterm.js, the yolo confirmation (Esc, Enter, no second ask), the
status bar colors, the transcript notice, a bash call under `yolo` without a
prompt and under `manual` with one, a pending ask closed by switching to
`yolo`, the `/permissions` picker (sources, filter, Up/Down, Shift+Tab, Esc,
focus back to the input, `/permissions <mode>`), Shift+Tab in the command
menu, a mode chosen before the session exists, a bundle mode from a project
bundle whose Bun `permission.approve` hook allows `echo` and defers `ls`,
and about 80 columns.
