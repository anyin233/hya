# 0.41.0

## TUI views: `/diff`, `/mcp`, `/rules`, `/agent-models`

- `/diff` shows the working tree's changes (`git diff HEAD` plus untracked files) full screen. The file list shows `+N -M` counts and the diff lines are colored. `n`/`p` or `]`/`[` switch files, `r` reloads, and the usual keys and the mouse wheel scroll.
- `/mcp` lists MCP servers with their state, tool count, and error. Enter shows a server's tools; `c` connects and `x` disconnects. `a` starts auth: it copies the authorization URL and asks for the code.
- `/rules` lists saved permission rules; `d` deletes one after you confirm.
- `/agent-models` lists each agent's effective model and where it comes from (session, configured, remembered, or default). Enter sets an agent's default model from the model picker, and `c` clears it. Agents whose model is fixed in config show why they can't be changed. See [TUI](docs/tui.md).

## Revert and redo sessions with file restore; fork at a message

- `POST /v1/sessions/{session}/revert` now works:
  - `{}` hides the last user message and everything after it, and puts back the files those turns changed. Call it again to go further back.
  - `{"messageId": …}` reverts to that user message.
  - `{"undo": true}` brings the messages and the file changes back.
  - The next prompt or shell turn makes the revert permanent. Hidden messages are left out of `ListMessages`, the model context, and titles.
  - The response lists each file as `restored`, `deleted`, `unchanged`, `skipped`, or `failed`. A revert while a turn runs returns `session_busy`.
  - `SessionInfo.revert` describes a pending revert, and the stream carries `sessionReverted`.
- File changes are recorded per tool call as `FilesChanged` events, with the earlier file contents stored per session in the event store and deleted with the session:
  - `write`, `edit`, and `apply_patch` are recorded in any directory.
  - `bash` (including your own `!` commands) is recorded only inside a git work tree.
  - Limits: 2 MiB per file and 256 MiB per session.
- Fork: `POST /v1/sessions/{session}/fork` with `{"messageId": …}` forks before that user message and returns its text as `promptText`. `untilSeq` is now honored. A fork of the latest state now keeps the last message; before, it was dropped. `SessionInfo.forkedFrom` names the source session. See [Protocol guide](docs/protocol/README.md).
- The projection reducer version is now 6. Existing logs replay unchanged.

## Provider View in the TUI

- `/key` (no arguments) opens a full-screen Provider View. The list shows each provider's protocol, key source, status, and model count. Open a provider to see its models with their display name, source (`remote`, `config`, `override`), limits, and reasoning. Changes apply at once, with no restart.
  - `a` adds a provider through a pop-up that asks for the name, then the protocol (`openai`, `openai-response`, `anthropic`, `google`), the base URL, and the key. It then fetches the provider's models. A failed fetch still adds the provider and shows the reason.
  - `k` sets or replaces a key (the input is hidden), `x` removes it, and `r` re-fetches the model list.
  - `t` tests the highlighted model by sending `hi` with a 1-token limit and shows whether it replied, the finish reason, and the latency. Esc cancels a running test.
  - `m` adds a model and `e` edits a model's display name, limits, or reasoning; both are written to `config.yaml`, and only the fields you change are saved. `d` removes a model's config override.
  - If the session would still run on the offline model after you add a provider, the `/model` picker opens with the new provider's models.
- Removed: `/keys`, `/login <provider>`, and `/key set|remove <provider>`. See [TUI](docs/tui.md#provider-view).

## TUI: dividers for earlier compactions, and live prompts from other sessions

- An opened session now shows a `context compacted` divider before every compaction summary, including compactions that happened before the TUI opened it (`--session`, `--continue`, `/open`, `/sessions`, and subagent views).
- A permission or question prompt from another session on the same server now appears as soon as it is raised. The TUI subscribes to `GET /v1/events/stream` for this. The status line reads `Permission needed in <n>. <session> · /open <n> to answer there`, and the pending block names the session. It also sends a desktop notification while the TUI is unfocused. You answer the prompt after opening that session. See [TUI](docs/tui.md).

## Providers can be managed live; model lists are cached in `model_cache.db`

- New v1 routes to manage providers without restarting the server:
  - `PUT /v1/providers/{providerId}` adds or updates a provider (`kind`, `baseUrl`, optional `apiKey`) and fetches its models.
  - `POST /v1/providers/{providerId}/refresh` re-fetches the provider's models from its remote.
  - `PUT` / `DELETE /v1/providers/{providerId}/models` adds or edits a model entry in `config.yaml` (name, context and output limits, reasoning), or removes it. An edit changes only the fields it sends; an empty name or a `0` limit removes that field.
  - `POST /v1/providers/{providerId}/test` sends `hi` to one model with a 1-token output limit (16 for Responses-style providers, which reject anything smaller) and reports whether it answered.
  - gRPC has the same calls. See [Protocol guide](docs/protocol/README.md#providers-and-keys).
- Saving or removing a key (`PUT` / `DELETE /v1/auth/{id}`) now takes effect immediately; a restart is no longer needed. Keys are written to `auth/<id>.yaml` with mode 0600. Provider rows now show `kind`, `baseUrl`, `keySource` (`saved`, `oauth`, `config`, or `none`; the key itself is never returned), and `modelCount`. Each model row now shows `source` (`remote`, `config`, `override`, or `offline`) and a display name.
- Model lists fetched from providers are stored in `$XDG_CACHE_HOME/hya/model_cache.db`, which replaces `models.yml.cache` (imported once). A provider's models are the fetched models plus the `models:` in `config.yaml`, merged by model id. Fields set in the config win and unset fields keep the fetched values. Listing models in `config.yaml` no longer turns off fetching. Model entries accept `name` and `reasoning: true|false`. Editing through the API rewrites `config.yaml`. See [Configuration](docs/configuration.md).
- `hya models --refresh [provider]` re-fetches models; `--verbose` shows each model's source and limits.
- Discovery also reads display names and limits from Anthropic, Google, and OpenRouter-style model lists.

## Desktop notifications

- While the terminal is in the background, the TUI sends a desktop notification when a turn finishes or fails, or when a permission or question prompt appears. It uses OSC 9 and OSC 777 and detects focus through terminal focus reporting. Turn it off with `/notifications off` (saved in `tui.json`). See [TUI](docs/tui.md#desktop-notifications).
- The WebUI shows these as browser notifications while its tab is hidden or unfocused. It asks for permission on your first click or key press. A notification that arrives as both OSC 9 and OSC 777 appears once. See [ADR-0021](docs/adr/0021-webui-desktop-notifications.md).

## TUI copy, external editor, and vim mode

- Copy: drag with the mouse to select text; it is copied to the clipboard when you release (OSC 52), and the status line shows `Copied N chars`. `/copy` copies the last assistant reply. On a terminal without OSC 52 the TUI reports `Copy failed`.
- External editor: `/editor` or Ctrl+X Ctrl+E opens the input in `$VISUAL`, else `$EDITOR`, else `vi`. The edited text goes back into the input without being sent.
- Vim mode: `/vim` (or `/vim on|off`) turns on vim keys in the input and saves the choice in `tui.json`. It supports normal and insert mode, the common motions, `dd`/`dw`/`cw`/`yy`/`p`, counts, `u` and Ctrl+R. The status bar shows `-- INSERT --` or `-- NORMAL --`. With vim on, Esc in insert mode only switches to normal mode; press it again in normal mode to cancel a turn or deny a prompt. See [TUI](docs/tui.md#vim-mode).

## Your own `!` commands run without a permission prompt

- A shell command you type yourself (`!cmd` in the TUI, or a v1 `ShellTurn`) no longer asks for permission in any mode: `manual`, `yolo`, or a bundle mode. Explicit deny rules still block it, a working directory outside the project still asks, and a plugin's `tool.execute.before` veto still applies. A plugin's `permission.ask` hook and a bundle mode's `permission.approve` approver are not consulted for it. Commands the model runs through `bash` ask as before. See [Configuration](docs/configuration.md).

## TUI themes and a preferences file

- `/theme` opens a picker of the built-in themes: `hya` (the default look), `light`, `contrast`, and `ember`. Moving the highlight previews a theme live; Enter keeps it; Esc restores the previous one.
- The chosen theme is saved in the TUI preferences file, `$XDG_CONFIG_HOME/hya/tui.json` (else `~/.config/hya/tui.json`). Set `HYA_TUI_CONFIG` to use another path. A missing or corrupt file falls back to the defaults, and the TUI shows a warning in the status line. See [TUI](docs/tui.md#themes).
- CI now runs the TUI unit tests and the browser TUI suite.

## `hya` starts the TUI and the WebUI

- Running `hya` in a terminal now starts three things: a server inside the `hya` process, the TUI in the terminal, and the WebUI at `http://127.0.0.1:3250`. Pick another port with `hya --port <N>`; `0` picks a free port.
  - The terminal TUI and every WebUI tab use the same server, so sessions show up in both at once.
  - `--db`, `--yolo`, and `--model` apply as they do for `serve`. The default database is the durable `$XDG_STATE_HOME/hya/sessions.db`.
  - Quitting the TUI (Ctrl+C twice, Ctrl+D, `/exit`, or a signal) stops the WebUI, its tabs, and the server.
  - This needs Bun on `PATH`. Without a terminal, `hya` still prints the guidance banner.
- The TUI shows the WebUI address in the status bar, the sidebar, and `/status`. When the WebUI can't start, for example because the port is taken, the TUI shows `WebUI unavailable: <reason> · hya --port <N>` and otherwise works as usual. New TUI flags: `--web-url` and `--web-error`.
- Bare `hya` writes server and WebUI output to `$XDG_STATE_HOME/hya/hya.log` instead of the terminal.
- The WebUI host now ends every tab's process (SIGHUP, then SIGKILL) when it gets SIGINT, SIGTERM, or SIGHUP.
- The TUI package is self-contained. `gen-api` now also generates its `/api` catalog into `packages/hya-tui/src/operations.json`. See [ADR-0020](docs/adr/0020-bundle-tui-and-webui-in-hya.md).
- Release archives and `install.sh` now ship the TUI (`lib/hya/tui`, with the target's OpenTUI native package) and the WebUI host (`lib/hya/tui-web`), each with production dependencies only, so bare `hya` works from an installed layout.
- The release smoke test and `xtask release-rehearsal` check the staged TUI and WebUI: required files and dependencies are present, no dev-only packages are included, `--help` runs from outside the checkout, and every import resolves inside the staged directories. The rehearsal also checks that the WebUI page and its assets are served.
- Fix: `xtask release-rehearsal` built a package named `hya` that doesn't exist; it now builds `hya-backend`, like the release workflow.

## Live assistant text and turn errors on the v1 stream

- v1 streams (SSE and gRPC, session and global) now deliver assistant text as it is generated. The live frames are `partStarted`, `partAppended`, and `partCompleted` with no `seq`, and they use the same message and part ids as the durable events. Before, text arrived only when the round finished. Each part's final text arrives as a durable `partReplaced` stream event, which is also sent when reasoning is replaced. Live frames cannot be replayed. Streams filter by `sinceSeq` but do not replay history, so after a reconnect a client re-reads the projection or calls `ListEvents`. See [Protocol guide](docs/protocol/README.md).
- A failed turn now records its error. A durable `error` event names the failed message and streams as `errorReported {message, code, errorMessage}`. The same error appears in `MessageInfo.error {code, message}` and in `TurnInfo.errorCode` / `errorMessage`, for example `provider_error`. The projection reducer version is now 3.
- `GET /v1/interactions` and gRPC `ListInteractions` without a `type` filter now list every pending interaction. Before, they returned nothing.
- `hya-sdk-v1` `V1SessionMirror` now:
  - de-duplicates parts by id;
  - applies `partReplaced` and `errorReported`;
  - ignores live deltas that are already covered by durable text;
  - records the message role and finish cause.
- Protocol docs now state that:
  - the turn id returned by `CreateTurn` is the user message id;
  - a prompt sent while a turn runs gets `409 session_busy`, because the server does not queue prompts.
- v1 `MessageInfo.agent` and `MessageInfo.model` are now filled per message, so older messages keep their own attribution after a `/model` or agent switch. `model` is the model that served the message's latest round, after `chat.params`, fallback, or routing; before any round reports usage, it is the model the turn requested. `MessageInfo.timeCreated` and `timeUpdated` are now set. The live `messageStarted` event carries the turn's `agent` and requested `model`, and `hya-sdk-v1` folds them. `Event::MessageStarted` gains optional `agent`/`model` fields, omitted when unset, so older logs replay unchanged. The projection reducer version is now 4. Messages recorded before this change have no agent.
- v1 tool parts now carry the whole call. `ToolCallPart` fills `callId` and `inputJson` in every state. On success it adds `outputJson` (the stored, size-capped output) and `durationMs`; on failure it adds `errorMessage`. A finished tool is still one part.
- v1 streams deliver tool calls as they happen:
  - `partStarted` gains `tool` and `callId`.
  - Argument JSON fragments stream as durable `partAppended` events.
  - `toolStateChanged` gains `tool` and `inputJson` when the tool starts running, `outputJson` and `durationMs` on success, and `errorMessage` on failure.
  - Behavior change: the stream's tool `errorCode` now matches the part's (`error.type`, else `unknown`). Before, it was always `tool_error`.
- Subagents are visible on v1. A durable `memberUpdated` stream event (`MemberInfo {member, child, agent, description, status, summary, callId, depth}`) is emitted on the parent session, and `SessionInfo.members` lists the folded rows. `callId` links a task tool card to its child session, as does `metadata.sessionId` in the task tool's output.
- A permission `Interaction.payload` now names the decision (`action`, `resource`, `always`) and the tool call that asked (`messageId`, `callId`, `tool`, and `input` with the call's arguments). The `GET /v1/interactions` title includes the resource again. The legacy `permission.asked.properties.tool` object gains optional `name` and `input`.
- `hya-sdk-v1` `V1SessionMirror` folds tool state frames and member updates.
- Subagents spawned by `task` now record the spawning call id and the task's `description` on their member row. v1 `memberUpdated.callId` and `SessionInfo.members[].callId` therefore link a member to its tool card.
- A resident member row now moves through a full status lifecycle:
  - It becomes `running` when an episode's first turn starts, and stays `running` while idle between wakes.
  - It then reaches one terminal status: `done` or `failed` from the member's report (now streamed as `memberUpdated`), `cancelled` when it is archived or stopped, or `failed` on a budget kill or a failure finalization.
  - A revived member goes back to `running`.
  - Hosts that embed `hya-core` must now pass a `TaskSpawnOrigin` to `spawn_resident_typed` instead of the subagent type string.
- Fix: cancelling or crash-recovering a parent turn no longer marks subagents whose `task` call already returned as cancelled.
- `GET /v1/interactions` and gRPC `ListInteractions` now return question interactions with the same `options`, `detail` (header), and `payload` as the live `questionRequested` frame. The frame's `payload` now carries every question: `{questions: [{question, header, options: [{label, description}], multiple?, custom?}]}`.
- Session event streams take an opt-in `includeDescendants=true` (gRPC `StreamSessionEventsRequest.include_descendants`). With it, permission and question asks from subagent sessions at any depth, and their resolutions, arrive on an ancestor's stream, tagged with the asking session. Without it, a stream behaves as before.
- `ModelSummary` reports each model's `contextLimit` and `outputLimit`.
- New usage fields, and a stream event per provider call:
  - `MessageInfo.usage` is the message's billed sum. `MessageInfo.roundUsage` is its latest round, used for context occupancy: `input + cacheRead + cacheWrite` against the model's `contextLimit`.
  - `SessionInfo.usage` is the session total, including title and summarizer calls. `TokenUsage` gains `reasoningUnknown`.
  - Each billed provider call streams as a durable `tokensRecorded { message, model, usage }`.
- Todo lists are now recorded as events. A change made by a todo tool records `todos_updated`; `GetSessionTodo` reads the projection, and the stream carries `todoUpdated { items }`. Sessions created before this version still read their list from todo tool results until their next todo edit.
- Fix: after a restart, the next todo edit builds on the recorded list instead of an empty one.
- `/compact` (`CompactSession`, and `SummarizeSession`) now records the same compaction event as automatic compaction. `compactionApplied` gains `message` (the summary divider), `foldedCount`, and `manual`. The projection reducer version is now 5, so cached projections are rebuilt once on upgrade.
- `hya serve` titles a root session automatically after its first prompt or command turn. The `title` agent generates the title in the background, using its configured model or else the session's model, so the turn is never delayed. The title streams as `sessionUpdated.title`, and the call is billed as `purpose: title`.
  - Skipped for subagent sessions, for sessions given a title at creation or by rename, for later turns, after restarts, and for shell turns. A rename made while a title is being generated wins.
  - Offline sessions get their prompt's first line as the title.

## Session permission modes

- Each session tree now has a permission mode, switchable at runtime:
  - `manual`: approval requests go to the user.
  - `yolo`: every tool action is allowed, the same as `--yolo`, but for this session tree only.
  - Bundle-declared modes, selected as `<bundle-id>/<mode-id>`.
- Setting and listing modes: set one with `PATCH /v1/sessions/{id}` `{"permissionMode": "yolo"}` (`Session.UpdateSession`). List the available modes with `GET /v1/permission-modes` (`Catalog.ListPermissionModes`). The contract now has 84 rpcs.
- Scope and default: the mode is stored as a `session_permission_mode_set` event on the root session, so it survives restarts, and subagent sessions inherit it. Without a recorded mode, a session is `yolo` when the server runs with `--yolo` or `permission.model: danger`, and `manual` otherwise.
- When a switch takes effect: from the next permission check, including checks inside turns that are already running. Switching to `yolo` allows the tree's pending permission requests once.
- `SessionInfo.permissionMode` reports the effective mode. `SessionUpdated.permissionMode` is streamed on the root session.
- A server started with `--yolo` or `permission.model: danger` now really asks for sessions switched to `manual`. Sessions that are never switched behave as before.
- Bundles can declare approval modes with `permission_modes: [{id, title, description?}]`. Such a bundle also needs `extensions.process` and a `permission.approve` hook resource. While a bundle's mode is active, its new `permission.approve` hook (params `{session, root_session, agent?, mode, action, resource}`) answers `allow_once`, `allow_always`, `reject`, or `defer`. A `defer` answer, an error, or a timeout falls through to the user. The Bun adapter supports the hook, and `hya bundle info` prints one `permission_mode=` line per mode. See [Configuration](docs/configuration.md) and [AgentBundle authoring](docs/agent-bundle-authoring.md).
- `PROJECTION_REDUCER_VERSION` is now 2, so stored projection snapshots are rebuilt once from the event log. Older binaries fold the new event as `unknown`.
- The `permission_modes:` example in the bundle authoring guide now includes a working `approver.ts`. The script speaks plugin protocol v1 itself, because an explicit `extensions.process` command runs as written, without the Bun adapter. The guide's `apis:` and `permission_modes:` examples are now tested; the test runs the example script under Bun where Bun is installed.

## Bun/OpenTUI frontend

- New interactive TUI in `packages/hya-tui`, adopted from a contributor fork ([ADR-0019](docs/adr/0019-adopt-opentui-frontend.md)). It is a Bun/OpenTUI client over the v1 HTTP/JSON+SSE contract. The screen shows sessions, the selected transcript, and pending interactions, plus views for models, Workflows, saved provider keys (`/keys`, `/key set|remove <provider>` with concealed entry), and a generic `/api METHOD /v1/path [JSON]` command. Tab completes slash commands and their arguments, and a footer row shows the next step for the current view. Run it from source with `bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080 --dir "$PWD"` against `hya serve`. It is not in the release archive. See [OpenTUI frontend](docs/tui.md).
- `packages/hya-tui-web` gains `e2e/hya-tui.spec.ts`: Playwright drives the real TUI in Chromium against an isolated `hya serve` on the offline model. The spec checks the connection, panel layout, prompt admission and reply, and Tab completion.
- The TUI now uses `@opentui/solid` 0.5.12 (`solid-js` 1.9.12) and requires Bun 1.4.2. The code is split into a state store, a controller, a slash-command registry, a key-binding table, and Solid components. The look, commands, keys, and CLI flags are unchanged. A new browser spec (`e2e/hya-tui-commands.spec.ts`) locks the colors, `/help`, `/models`, `/api`, concealed `/key set` with Esc, narrow widths, and Ctrl+C.
- Browser specs can script the model. `packages/hya-tui-web/e2e/fake-model.ts` is a fake OpenAI-compatible server that streams text in timed chunks and can emit tool calls, HTTP errors, and hangs you release later. The `backend` fixture's `model: { steps: [...] }` option points the isolated `hya serve` at it. Without the option, specs keep using the offline echo model.
- The TUI streams assistant replies into the transcript chunk by chunk. When the round ends it switches to the server's stored copy without doubling or flicker. After a resync or reconnect it fills the gap from `ListEvents`.
- Prompts typed while a turn runs are queued, shown dimmed under `user · queued`, and sent in order after the turn ends. The TUI retries `409 session_busy` with a short backoff.
- After a turn, the status line shows `Ready`, `Cancelled · Ready`, or `Error · <code>: <message>`, instead of staying on `turn_state_running`. A failed assistant message shows its error in the transcript.
- New layout: a single main column with a toggleable sidebar holding Sessions, Todos, and Context. The sidebar shows automatically at 110 columns or wider; Ctrl+B or `/sidebar [on|off]` pins it. Pending interactions appear as a compact block above the input.
- Messages render by role: user messages as blocks with an accent bar, assistant messages under an `agent · model` header. Assistant text renders as Markdown, using OpenTUI's `<markdown>`, with syntax-highlighted code blocks. Errors, cancellations, and length-limit stops show as colored notices.
- Reasoning collapses to a `Thinking` line; Ctrl+O, `/thinking [on|off]`, or a click expands it. The transcript scrolls with PgUp/PgDn, Ctrl+Home/End, and the mouse wheel, and shows a "new messages below" hint when scrolled up. The projection is re-read at least every 400 ms while frames keep arriving.
- The browser test fake model also speaks the OpenAI Responses API (`model: { steps, protocol: "responses" }`), with `reasoningStep` and `textStep(..., { finish: "length" })`.
- The input is a multi-line editor. Enter sends; Ctrl+J or Alt+Enter insert a newline. Shift+Enter also inserts one in terminals that report it, but in the browser WebUI it sends, because xterm.js has no kitty keyboard support. Pasted text never sends. The box grows to 8 rows, and Up/Down recall earlier inputs.
- Esc cancels the running turn (`Cancelling…`, then `Cancelled · Ready`); with no turn running it clears the input. Ctrl+C clears the input and quits on a second press within 2 seconds. Ctrl+D on an empty input and the new `/exit` and `/quit` commands also quit.
- `!command` runs a shell turn, shown as `!command`, `↳ bash`, `$ command`. Typing `@` suggests files from the work directory (`FindFiles`) and inserts `@path` into the prompt.
- Typing `/` opens a command menu with fuzzy filtering. It merges local commands with the backend's command and skill catalog (tagged `[local]`, `[command]`, or `[skill]`); a local command wins when names clash. Up/Down select and Tab completes the name. Enter runs the command when its arguments are optional, and otherwise completes it and waits for arguments. Esc closes the menu.
- Skill and server commands (`/<skill> args`, `/init`, `/review`) show what you typed in the transcript, not the expanded template.
- New commands: `/agent`, `/rename <title>`, `/compact`, `/summarize`, `/todos`, and `/status` (server, version, directory, session, agent, model, and permission mode). With no argument, `/model` and `/agent` show the current value and the available choices.
- Tool calls render as cards with a state icon: pending, a running spinner, awaiting approval, done, or failed. Each card shows a per-tool summary and the duration. Edit, write, and patch cards show colored diffs, and long output is cut to its head and tail. Ctrl+G or `/tools [on|off]` expands or collapses all cards; clicking a card header toggles that card. `!command` shell turns use the bash card and show their output.
- `task` cards show the subagent's status and latest activity. Clicking a card or `/open <child>` opens the child session read-only, with a banner, and Esc returns to the parent. The sidebar nests subagent sessions under their parent.
- The browser test fixture gains a `model.permission` option (`default`, `allow`, or `danger`).
- Permission prompts appear docked above the input, while the input is empty. Each prompt shows the waiting call the way its tool card does (the command, the diff, or the path or URL) and who asked.
  - `1` allows once, and `2` always allows (the prompt shows what "always" covers).
  - `3` and Esc deny; Esc never approves, and typing in the input never answers a prompt.
- `ask_user` questions appear as prompts with selectable options, a free-text answer typed into the input, and Reject. Several pending asks show one at a time (`1 of N`).
- A subagent's permission request or question shows in the parent view, labelled with the subagent. Its task card and sidebar row show that it is waiting. `/approve`, `/deny`, and `/answer` remain as keyboard fallbacks.
- While a turn runs, a working line shows a spinner, the elapsed time, and the current activity: `Thinking…`, `Writing…`, `Running <tool> <summary>`, waiting for approval, an answer, or a subagent, plus any queued prompts. It ends with an Esc hint. A status bar under the header shows the permission mode, the directory, the git branch, and the connection state.
- The sidebar's Todos box is live and marks each todo's status with a glyph. When the sidebar is hidden, the status bar shows a compact `Todos n/m`. The sidebar's message count no longer sticks at 0.
- `CompactionApplied` renders as a transcript divider, and engine system messages render as muted notices instead of assistant blocks. The TUI shows a notice on connect when the backend version differs from its own.
- Switch the session's permission mode with Shift+Tab, which cycles manual → yolo → bundle modes, or with the `/permissions` picker; `/permissions <mode>` switches directly.
  - The first switch to yolo in a TUI process asks for a one-line confirmation.
  - Switching to yolo closes pending asks immediately.
  - A mode chosen before a session exists is applied when the session is created.
- The status bar colors the mode: `⚠ yolo` in red, and bundle modes by their title in the accent color. Each switch adds a `Permission mode → …` notice to the transcript, and the permission prompt's hint names the current mode. The status line no longer repeats `Running · msg_…` while the working line is shown.
- A reusable modal picker (filterable, keyboard and mouse). The browser test fixture gains `projectBundles` and `approverBundle()` for bundle-provided permission modes.
- New `/model` and `/agent` pickers: rows are tagged with the provider or default model, and the current value is marked. The direct forms (`/model <provider/model>`, `/agent <name>`) still work. A choice made before any session exists is applied to the next session.
- A `/sessions` picker lists a New session row, then the sessions with subagents nested under their parent, each with its last update time. F2 renames the highlighted session. Ctrl+D deletes it after a confirmation.
- Session titles in the header, sidebar, and pickers update live from `sessionUpdated`, not only on refresh.
- One-command launch. Without `--server`, the TUI starts its own `hya serve` and stops it on every exit path.
  - The `hya` binary comes from `--hya`, then `HYA_BIN`, then `PATH`. The durable default database is `$XDG_STATE_HOME/hya/sessions.db`.
  - New flags: `--continue`, `--session <id>`, `--hya`, and `--db`.
  - Without `--continue` or `--session`, no session is opened at start; the first prompt or `/new` creates one.
- A key and command help overlay opens with `?` on an empty input, or with `/help`. It is generated from the key binding tables, grouped by area, and filterable. Commands are tagged local, server, or skill.
- The status bar shows context occupancy (`ctx N%`, in the warning color from 80% and the error color from 95%) and the session token total. The todo panel updates live from `todoUpdated`. `/compact` and automatic compaction show a `── context compacted · N messages · manual ──` divider before the summary.
- Subagent permission and question prompts arrive at once on the parent session's stream (`includeDescendants=true`). The interactions listing is no longer polled.
- The browser test fake model answers background session-title requests on its own, so they don't consume scripted steps.

## List saved provider keys over the v1 API

- New rpc `Auth.ListProviderAuth` (`GET /v1/auth`) answers `{"providerIds": [...]}`, the sorted provider ids with a saved `auth/<id>.yaml` credential. It never returns key values. The contract is now 17 services / 83 rpcs.
- `PUT /v1/auth/{providerId}` now writes the key file with mode `0600` on Unix.
- HTTP GET list routes read nested pagination from `page.cursor` and `page.limit` query parameters. Before this, these keys were ignored.

## Browser-rendered TUI test environment

- New package `packages/hya-tui-web` runs a terminal program on a real PTY and renders it in the browser with xterm.js. Start it with `bun packages/hya-tui-web/src/main.ts [--host 127.0.0.1] [--port 7681] [--cwd DIR] -- <command...>`. Each browser connection gets its own process. Browser resizes reach the program as SIGWINCH. The host rejects cross-origin WebSocket upgrades and binds loopback by default.
- The `/pty` WebSocket speaks the protojson form of the `hya.v1` `PtyClientFrame`/`PtyServerFrame` messages (`input`, `resize`, `ping` / `output`, `exit`, `pong`).
- A Playwright harness (`e2e/harness.ts`) drives TUIs in Chromium. It reads the screen text and per-cell color and width, sends keys, resizes the viewport, waits for the exit code, and attaches a screenshot for every test. An OpenTUI probe fixture checks borders, truecolor, wide glyphs, input, resize, and Ctrl+C. See [Browser-rendered TUI](docs/tui-web.md) and [ADR-0018](docs/adr/0018-browser-rendered-tui-test-environment.md).

## Every bundle has one `config.yml`

- Each bundle now reads its configuration from one file. For bundles installed for the user, and for the builtin first-party bundles, the file is `<hya config dir>/bundles/<percent-encoded-bundle-id>/config.yml`. The `<hya config dir>` is the directory that holds the active `config.yaml`. For example, `hya/plan-impl-review` reads `~/.config/hya/bundles/hya%2Fplan-impl-review/config.yml`. A project bundle (`hya bundle install --project`) reads `config.yml` in its own `.hya/bundles/<dir>/` source directory.
- **Breaking:** Bundle Agent model defaults (`agents.<agent-id>.model`) now live in this file. Hya no longer reads the old `<hya config dir>/agents/<encoded-bundle-id>/config.yml`: move each file to `bundles/<encoded-bundle-id>/config.yml`. Saves still change only the model leaf and keep every other key, so a bundle can store its own settings in the same file.
- `extensions.process` providers, bundle MCP stdio servers, and agent sidecars all receive the absolute `HYA_BUNDLE_CONFIG_DIR` and `HYA_BUNDLE_CONFIG_FILE` paths. The file does not have to exist. Process argv, MCP argv, and MCP `env` values also expand `${BUNDLE_CONFIG_DIR}` and `${BUNDLE_CONFIG_FILE}`. Bundle MCP servers get the inherited `PATH` and `HYA_BUNDLE_ROOT` as well. A key declared in the MCP `env` map overrides the config variables and `PATH`.
- If you edit the `config.yml` of a bundle that runs a process or MCP server, that bundle's providers restart at the next root binding, the same as when the bundle itself changes.
- A project bundle's `config.yml` is not bundle content. It doesn't enter the bundle's sources, digest, or project fingerprint. A reinstall or upgrade keeps the existing file, and an incoming package never writes one.

## Plugin hooks reach bundle agents, and `chat.params` knows the request chain

- An installed Plugin's hooks now run for bundle-defined agents too, not only for built-in agents. A bundle agent's chain is every installed Plugin's hooks (ascending bundle id), then its own bundle's hooks filtered by `hook_refs`, then its activation sidecar's hooks. A Plugin's `chat.params`, `tool.execute.before` veto, and `permission.ask` answer therefore apply to every session. Subagent and resident members keep their sidecar hooks when Plugin hooks join their turn.
- `hook/chat.params` params gain two optional fields: `root_session`, the root of the session's spawn tree (equal to `session` for a root), and `agent`, the session's stable agent id. A plugin can use them to keep one decision per request chain. Plugins that ignore unknown fields keep working, and the hook's outcome is unchanged. The Bun adapter passes both fields to `chat.params` handlers.

## Plugins can choose the fallback model when a provider fails before streaming

- New hook `model.fallback` (`hook/model.fallback`, posture Open, always fail-open). A provider can fail before any stream exists. When the configured `categories:` chain can no longer advance, the engine asks this hook for the next model. The params are `session`, `root_session`, `agent`, `message`, the failed `model`, `error` `{ class, message }`, `attempt`, and `tried`. The error class is one of `retryable`, `unknown_model`, `auth`, `invalid_request`, or `other`. The hook answers `{ "outcome": "retry", "model": "provider/model" }` or `{ "outcome": "give_up" }`, and the first `retry` wins.
- Safety limits: a model already tried in the round is refused, a round makes at most 8 attempts, and the hook is never called once a stream exists. Workflow-routed turns don't call the hook. No new events are recorded; each switch logs a warning.
- Process-backed bundles (`extensions.process`) and configured plugins can declare `model.fallback`. The Bun adapter registers and dispatches it: a handler returns `{ outcome: "retry", model }`, a bare model string, or `{ outcome: "give_up" }`.

## Optional `hya-extra/*` bundles

- New `bundles/extra/` ships four optional bundles. None is installed by default: package one with `cargo run -p xtask -- package-bundle bundles/extra/<name> <name>.hyabundle`, then run `hya bundle install`. Each also serves as a coverage fixture, checked by `crates/hya-bundle/tests/extra_bundles.rs` and e2e rows T2.26–T2.28. See [hya-extra bundles](docs/extra-bundles.md).
- `hya-extra/zvec-grep` (Plugin): gives agents [zvec-grep](https://github.com/zvec-ai/zvec-grep) semantic workspace search. It runs `zg server --stdio` as a bundle MCP server and ships a usage skill. It needs `npm install -g @zvec/zvec-grep`.
- `hya-extra/scout` (AgentSetBundle): the `scout` subagent, a cheap-model retrieval scout (`model_policy.category: quick`). It uses its own zvec-grep MCP server plus `read`/`grep`/`glob`, and returns answers with `path:line` evidence.
- `hya-extra/jev-model-router` (Plugin, Bun process, `chat.params`): asks TypeSafe's Jev model how hard a request is, then rewrites `request.model` to the matching configured tier. The decision sticks to the request chain (`root_session`) so prompt caches stay warm. It falls back to `default_tier` when Jev fails or is unsure. Configure it in its bundle `config.yml`.
- `hya-extra/model-fallback` (Plugin, Bun process, `model.fallback`): configured per-model fallback chains, for example `chains: { provider/a: [provider/b] }`. It retries the next model when a provider fails before streaming, limited by an error-class filter and `max_attempts`.
- `hya-extra/token-summary` (Plugin, Bun process): per-model token usage (input, cache creation, cache read, output split into thinking/visible) for a session tree, over its own session API endpoint (`GET /v1/sessions/{session}/bundles/hya-extra%2Ftoken-summary/usage`, JSON Schema `schemas/usage.json`, a bad `scope` answers 400) and an agent tool, `token-summary__token_summary`, that renders the same data as a Markdown table.

## Per-round token accounting with the serving model

- Every provider decoder now reports usage under one documented `TokenUsage` invariant: `input` is uncached prompt tokens (it excludes `cache_read` and `cache_write`), `output` counts every generated token including thinking, and `reasoning` is the thinking share of `output`. OpenAI Chat and Responses/Codex/Grok subtract cached tokens from `input`. Google adds `thoughtsTokenCount` into `output`. New `reasoning_unknown` flag (omitted when false): Anthropic, and OpenAI routes that don't send reasoning details, mark the thinking split as unknown instead of estimating it. See [providers](docs/architecture/providers.md#token-usage-normalization).
- New event `usage_recorded { session, message?, step?, model, purpose, tokens }`. It records every provider call that reports usage, with the model that actually served it (after `chat.params`, the fallback chain, the `model.fallback` hook, or a Workflow route). Turn rounds (`purpose: turn`) are recorded even when the message later ends `cancelled` or `error`, and even when the stream fails after reporting usage. Title generation (`title`) and summarizer calls (`compaction`: ladder summary/handoff, `/compact`, terminal handoff) are billed to their session with no message. Older binaries fold the event as `unknown`.
- `SessionProjection.usage` folds billed usage by serving model (`by_model`) and by purpose (`by_purpose`). Each `UsageTotals` holds `input`, `cache_read`, `cache_write`, `output`, `reasoning`, `reasoning_unknown_output`, `rounds`, and `legacy_messages`. `UsageTotals::output_split()` separates thinking, visible, and unknown-split output. The totals never shrink when messages are deleted or reverted, or when the transcript is compacted. For logs that have no per-round records, each message's `MessageFinished.tokens` counts once, under model `unattributed`. `MessageFinished.tokens` itself is unchanged. See [event model](docs/architecture/event-model.md#session-usage-fold).
- The usage ledger records the model that served the message and counts the whole prompt (`input + cache_read + cache_write`). It also keeps billed usage for cancelled and errored messages. Context-window occupancy now includes cache-write tokens.

## Bundles can register their own API endpoints

- New manifest key `apis:` for any bundle kind with an explicit `extensions.process` (prepare rejects endpoints without one): `{ id, method: GET|POST|PUT|PATCH|DELETE, scope: session|global, path, description?, request_schema?, response_schema? }`. Paths are templates such as `/items/{id}`: a leading `/`, 1–16 segments of `[A-Za-z0-9._-]+` literals or whole-segment `{name}` parameters, at most 256 bytes, no wildcards. Two endpoints with the same method and scope may not have overlapping templates (`/items/{id}` and `/items/latest` are rejected rather than ranked). Schema paths must name `extensions.files` entries holding a JSON Schema, so they are packaged and covered by the digest. At most 64 endpoints per bundle. The endpoints are stored sorted in a new document-level `apis` section of the prepared catalog, skipped when empty. `hya bundle info` prints one `api=<METHOD> <scope> <path> id=<id>` line per endpoint. See [AgentBundle authoring](docs/agent-bundle-authoring.md#api-endpoints-apis).
- New v1 service `BundleApi` (17 services / 82 rpcs): `ListBundleApis` (`GET /v1/bundle-apis`, every published endpoint with its schemas), `InvokeSessionBundleApi` (`/v1/sessions/{session}/bundles/{bundle}/{path…}`), and `InvokeGlobalBundleApi` (`/v1/bundles/{bundle}/api/{path…}`), each served for all five methods. The bundle id is one percent-encoded path segment (`hya-extra%2Ftoken-summary`), the query string and a JSON body (at most 512 KiB) reach the process, and the HTTP response is the process's own status (`200..=599`) and JSON body verbatim. Over gRPC the reply is `BundleApiResponse { bundle, api, status, content_type, body }`, and a process status is data, not a gRPC error. New stable error codes for host-side failures: `bundle_api_not_found` (404 / `NOT_FOUND`), `bundle_api_method_not_allowed` (405 with `Allow` / `UNIMPLEMENTED`), `bundle_api_bad_request` (400 / `INVALID_ARGUMENT`), and `bundle_api_failed` (502 / `UNAVAILABLE`). `gen-api` accepts a documented `ANY` binding for these passthrough rpcs and expands it into five OpenAPI operations. See [protocol guide](docs/protocol/README.md#bundle-api-endpoints).
- Plugin protocol v1, additive: the initialize reply can list `apis: [{ name, description? }]`, and a bundle process must list exactly its manifest endpoint ids or the bundle fails to start. The host sends a new request, `api/request { api, method, path, path_params, query, body, session?, call, host_capability }`, and the process answers `{ status?, body? }` (unknown fields rejected). The request uses the normal 30 s timeout, and each request is served by the live runtime generation, which stays alive until the request completes. `host/capability` `session` is now optional: a global request's capability has none. See [plugin protocol](docs/plugin-protocol.md#bundle-api-endpoints-apirequest) and [bundle runtime](docs/bundle-runtime.md#bundle-api-endpoints).
- Request-scoped host capabilities: tool calls into every installed bundle process now carry `host_capability`, whatever the process kind (`rust`, `bun`, `claude`), and so do API requests. Before this change only Rust processes got one. Configured plugins still never do. A new read-only operation, `session.usage { scope: session | tree | root }`, returns per-session and merged billed usage. Each session's usage is folded from its own replayed projection, the tree follows subagent spawn edges, and each model's totals include `split { thinking, visible, unknown }`. A session-scoped API request may read only its own session and that session's subagents; a global one has no session to read (`-32001`). `permission.assert` is tool-call only, so the capability stays read-only for every HTTP method: write endpoints change only the bundle's own state. `context.describe` now includes `request` (`tool_call` or `api`; API requests also report `api`, `scope`, and `call`).

## Provider error frames inside a 200 stream are retried before the first event
- Provider streams now classify error frames that arrive inside an HTTP 200 SSE body (Anthropic `{"type":"error","error":{"type":"overloaded_error"|"rate_limit_error"|"api_error"}}`, Gemini `RESOURCE_EXHAUSTED`, Responses `response.failed` codes, and gateway frames such as "Concurrency limit exceeded for account, please retry later") into their HTTP-equivalent status (429/529/5xx/400/401/403/…), with messages prefixed `in-stream error`. Transient classes are retried within the `provider_retry` budget and backoff (honouring an in-band `retry_after`, capped at 30 s) only while no event has reached the consumer; after the first delivered event the error surfaces exactly once (strict no-replay). Invalid-request, auth and unclassified frames are never retried. An `"error": null` key in a normal chunk no longer aborts the stream.
- Removed a leftover `[dbg-http] POST <url>` stderr line printed on every provider request.
## A session runs at most one turn at a time
- Fixed: a session now runs at most one turn at a time, enforced by the engine. A team-quiescence wake ("TEAM QUIESCED") or child mail that arrived while the lead's own turn was still streaming used to start a second, concurrent assistant turn on the lead, which duplicated delegation and rewrote files from stale state. Such wakes now queue and are delivered when the running turn ends, and quiescence is only declared once the lead is idle too. Concurrent `run_turn` calls on one session queue in order; a shell turn on a busy session fails with `session_busy` (409). Subagents are separate sessions and keep streaming concurrently. A main-actor wake whose mail was already consumed inside the running turn no longer runs an empty follow-up turn.
- Added: `SessionEngine::try_begin_turn` / `turn_active` / `set_turn_observer`, `TurnLease`, `TurnBoundaryObserver`, `CoreError::TurnAlreadyActive`.
## Tool schemas and errors guide the model to a valid call
- `edit`: op `replace` errors and `E_NO_MATCH`/`E_MULTI_MATCH` diagnostics name the exact valid field shape with a worked example and, for no/multi match, the candidate line number(s). `{"op":"replace","oldText","newText"}` without `pos`/`end`/`lines` is accepted as `replace_text` (that shape could never succeed before). A `#WN:`-style anchor with no line number is no longer misreported as line 0.
- `grep`: regex compile errors add a Rust-regex dialect hint (no look-around/backreferences, escape literal parentheses) and point at `literal: true`.
- `ls`: an empty `path` means the working directory; relative paths resolve against the session workdir (not the process cwd); read failures name the directory.
- Path normalization no longer collapses `.` + `.` into an empty path (affected every file tool when the workdir is `.`).
## Headless commands start under the configured `default_agent`
- `hya exec`, `hya run`, `hya -p` goal mode, `hya loop`, `hya rpc` and `hya workflow run` now start their root session under config `default_agent` (they always used `build` before; only `serve` honored it). An unknown or unselectable `default_agent` fails with the named id instead of silently falling back.
- A `config.yaml` that sets only `default_agent` (no providers/mcp/plugins/permission) is no longer discarded as an unusable offline config.
## Every turn ends with a terminal event; failed leads are never archived
- Fixed: stopping a run no longer leaves turns open or members orphaned. SIGINT/SIGTERM on `hya exec`/`run`/`-p`/`loop`, the normal end of those runs, and `hya serve` shutdown drain every in-flight turn in every session (lead, members, a late quiescence-synthesis turn). Each open assistant message gets exactly one `message_finished` (`finish: cancelled`), open tool parts get a `tool_error`, and members go terminal. The drain waits at most 5 s. `exec` exits 130 on SIGINT and 143 on SIGTERM; a second Ctrl-C exits at once.
- Fixed: turns left open by a crash (SIGKILL, OOM) are closed by the next `hya` process that opens the same database, before any turn runs (`cause: interrupted`), exactly once, via a new open-turn index (migration 0010, backfilled on upgrade).
- Added: optional `cause` on `MessageFinished` / `MessageInfo.finish_cause` (`user_cancel`, `shutdown`, `leader_failed`, `interrupted`, `provider_error`, `other`) in the event log and the `hya.v1` IDL. Old logs decode unchanged.
- Fixed: a failed lead turn no longer reports, hands off, and archives `main`. The turn ends with `finish: error, cause: provider_error` and the session stays live and resumable. Every live member is mailed a `LEADER FAILED … wrap up now` notice from `harness`, and the failed lead gets no automatic synthesis or mail-wake turn until the user resumes it. A user cancel never sends the notice.
- Fixed: `SessionInfo.busy` is true while any engine turn runs on the session, and `/v1` turn cancel stops engine turns (member sessions included) with `cause: user_cancel`.
## All subagents are resident; `spawn_lifecycle` removed
- **Breaking:** `spawn_lifecycle` is removed from every bundle manifest (AgentBundle, AgentSetBundle, WorkflowBundle agents, `bundle.hya.md`). A manifest that still sets it fails preparation with `` `spawn_lifecycle` was removed from the bundle manifest ``; delete the key. Every `task` spawn is a resident actor: it returns a handle at once, runs until it `report`s, and later mail wakes it. Bundles installed by earlier releases keep loading.
- **Breaking:** `hya/subagents` now ships one worker, `hya-worker`, replacing `hya-transient-worker` and `hya-resident-worker`.
- Workflows: a Stage's `actor` key alone selects a persistent actor Session; the "targets transient Agent", "resident Agent without an actor key" and "verifier must be transient" errors are gone. The task tool's `inline_agent.resident` flag and the Claude importer's `spawn_lifecycle: transient` line are removed.

## `archive` replaces `kill`
- **Breaking:** the `kill` tool is removed. `archive {target, reason?}` (permission `task`) stops a live subagent and archives it. The target may be its handle, its leaf name, or its session id. Its in-flight turn ends with `finish: cancelled, cause: archived`, its own subagents are archived first, and `AgentArchived.reason` is `archived_by_parent`.
- Archived subagents stay readable; mail to their handle or their DM channel wakes them with the same handle and session. Errors name the fix: an unknown target lists your live subagents, an archived one tells you to send mail to wake it, and the lead can never be archived.
- A graceful stop (end of a one-shot run, SIGINT/SIGTERM, `serve` shutdown) archives members (`reason: shutdown`) instead of marking them failed, so a later run on the same database can wake them. Added `FinishCause::Archived` / `FINISH_CAUSE_ARCHIVED`.

## `wait` for subagents
- Added `wait {targets?, mode: all|any, timeout_secs (default 600, max 1800)}`: blocks the calling turn until subagents finish their current work (report, go idle, or are archived) and returns `{woke_by, finished[], running[], mail[], waited_ms}`. It wakes inside the lead's own turn, and cancelling the turn aborts it. The team prompt tells agents to wait instead of polling.
- With the channel tools loaded (the default), `wait` also returns when mail arrives for the caller, harness notices such as LEADER FAILED included (`woke_by: mail`). Tool families can declare `overrides: <family>` in their exposure policy; channel-tools' `wait` replaces extended-tools' `wait` this way. Added `ToolRegistry::from_tool_families`.
## Every agent gets its coordination tools at startup
- **Breaking:** every agent, built-in or bundle, root or subagent, with or without a `resource_view`, gets its coordination tools when it starts. Subagents get `report`; every agent gets `wait`; agents that can spawn (a built-in, or a bundle `can_spawn` naming an installed agent) get `task` and `archive`; with the channel tools loaded, every agent gets `send`, `list_channel` and `read channel://<id>`. `resource_view` now narrows only domain tools (read, write, edit, bash, grep, glob, MCP, skills). An agent whose view has no `read` gets a mail-only `read` that serves `channel://` handles and refuses file paths. A bundle agent that can spawn nobody no longer sees `task` or `archive`. Listing a coordination tool explicitly still works and is deduplicated.
- **Breaking:** `resource_view.deny` can remove `task`, `archive`, `wait`, `send` or `list_channel`; denying `report` rejects the view with `InvalidManifest` (a subagent without it could never finish). Denying `read` removes file reading only.
- A rejected `report` names each channel holding unread mail and the exact call to read it (``read channel://DM-…``), then points to `send` and `wait`. Mail the current wake already delivered no longer counts as unread.
- `[NEW MAIL]` notices no longer lose mail when a busy team overflows the live event bus; after a lag the notice is rebuilt from the stored inbox.
- The team quick reference is added to every request and lists only the tools that agent has.
- `hya-extra/scout` lists only its domain tools; it can now read and answer its lead's mail and then report instead of being re-spawned.
## Large databases start fast; projection reads fold only new events
- `hya serve` on a large database is ready in seconds instead of minutes: on a 187 MB, 646k-event database with 8 resident actors left by a crash, 81–86 s → 1.2 s (restart) / 5–6 s (first open after upgrade), and readiness RSS drops from 450 MB to about 100 MB.
- Session projections are cached in-process and as durable snapshots (table `projection_snapshot`, migration 0011). A read decodes and applies only the events after the cached fold and always equals a full replay of the event log. Snapshots are derived data, tagged with `PROJECTION_REDUCER_VERSION` and anchored to their last event; they are discarded when the reducer version changes, when their anchor event is gone, or when the session is deleted. The first read of each existing session after upgrading folds its log once.
- The token-summary usage report (`session.usage`), `wait`, in-turn mail steering after a bus lag, the shutdown drain's member archiving, and `GET /v1/sessions` read cached projections: a 14-session usage tree drops from 4.2 s to 4 ms (0.3 s right after a restart), and `GET /v1/sessions` from 8.8 s to 0.3 s.
- `HYA_STARTUP_TRACE=1` emits per-phase startup marks; `cargo run -p xtask -- startup-bench --db <seed.db> [--timeout-secs N]` benchmarks cold listen on a copy of an existing database and prints the phase waterfall.
## Configured models can declare context and output limits
- An object-form model entry in `providers.<id>.models` can set `limit: { context, output }` (same names as `models.yml.cache`). When a model's output limit is known, every provider kind sends it as the default max-tokens value and clamps larger explicit requests to it, so Anthropic routes no longer cut such models off at 4096 tokens (a scout on `glm-5.3-flash` stopped with `finish: length` at exactly 4096). With no known limit, Anthropic keeps the 4096 fallback and other kinds still omit the field. Anthropic thinking budgets are shrunk, or thinking is omitted, so the request stays within the limit. Invalid limits (zero, negative, non-integer, above u32, `output` greater than `context`, unknown keys) are config errors. `limit.context` also replaces the 200k route default in the catalog and the compaction threshold.

## `wait` waits for real reports and never re-delivers mail
- Fixed: a subagent counts as finished for `wait` only when it reports or is archived, never when it merely goes idle, and a working or idle member's in-progress text is no longer shown as its `report`. A subagent woken by mail after its report is working again until its next report.
- Fixed: repeated `wait` calls no longer spin on the same state. Targets that finished before the call are listed under `already_finished` and never wake the wait; when every target already finished it returns at once with `nothing_to_wait_for`. A subagent whose turn ended without a report and has nothing queued wakes the wait once with `woke_by: stalled`, so the lead can `send` it a nudge or `archive` it; waiting again blocks. Only `timeout_secs: 0` returns the current state without blocking.
- Fixed: mail is delivered once. A `wait` that returns mail advances the durable inbox cursor (`MailConsumed`), so the same message never comes back from a later `wait`, a `[NEW MAIL]` notice, or a later wake of the lead. A subagent's report mail is delivered as its finish, not as a mail wake. `[NEW MAIL]` notices are rebuilt from the durable inbox, so mail on a DM channel created after a long lead turn began is steered too.

## An accepted `report` ends the member's turn
- An accepted `report` now ends the member's turn after that tool round, with no further model call; the round's other tool calls still complete and the message closes with a single `finish: stop`. A second `report` in the same turn is rejected with an actionable error, so each episode produces exactly one report. Mail that arrives during the report round stays unread and wakes a new episode. The acceptance text now reads "Report accepted; your turn ends now." (A real run had one implementer report 43 times in one turn and the reviewer 64 times.)

## Subagents are named `<subagent_type>-<operator>`
- **Breaking:** subagent handles are now `<agent>-<operator>`, e.g. `main/scout-suzuran` or `main/hya-implementer-exusiai/general-amiya`. Choose the agent with `subagent_type`; the harness derives the prefix from the resolved agent id (lowercased, other characters turned into `-`, capped at 32) and appends one random Arknights operator name from a checked-in list of 419 names (prts.wiki operator list, snapshot 2026-09-25, normalized to lowercase ASCII with `-` separators; "X the Y" variants as `x-the-y`). `task` has no `name` parameter: a call that passes one fails with an input error and spawns nothing. An omitted `subagent_type` still spawns `general`. A leaf is never reused within a team, live or archived; after collisions it falls back to two names (`scout-suzuran-amiya`). `HYA_HANDLE_SEED` makes names reproducible. Counter handles in existing logs keep replaying and resolving; scripts that hard-coded `main/<type>-1` must read the handle from the `task` result.

## `wait` results and long tool output keep their summary
- `wait` keeps its whole result under the tool output cap: it starts with why it woke, one line per target (handle, state, outcome), the still-running subagents and the new-mail count, then previews of each report and mail message sharing a fixed budget. A cut preview ends with `read channel://<DM id>` for the full report. Metadata keeps the structure (state, outcome, `channel`, `report_chars`, `report_truncated`, mail sender and size) without repeating the bodies.
- A tool result over 5000 characters now keeps its first 2000 and last 2500 characters with a marker naming the omitted size and the `artifact://` handle of the full output (before, only the last 5000 characters were kept, dropping headers and leading JSON keys). `bash`, `read` and `grep` keep their existing policy.

## `grep` clamps `context`; channels can be read by member handle
- `grep` clamps an out-of-range `context` into 0–5 instead of failing and says so in the result.
- `read channel://<member handle>` (or `read #main/scout-suzuran`) shows your DM with that member, with a warning naming the DM id; an unknown channel's error lists your own channels and points to `list_channel`. Real channel ids always take precedence.
