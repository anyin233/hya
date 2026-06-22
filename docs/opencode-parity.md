# OpenCode Parity Matrix

Last refreshed: 2026-06-22.

OpenCode baseline: `sst/opencode` `origin/dev`
`cd292a4ecbaeedd19239edddca77f86d9727c9ae`
(`chore: generate`; latest functional increment:
`35b3fc85d091594427a5344e2ad95128b62453b1`
`feat(core): expose session switching endpoints`).

yaca baseline: current `feat/yaca-pi-parity` branch with committed OpenCode
session, abort, file, instance metadata, VCS, prompt_async, session switching,
session context, session title/metadata/permission/archive update/delete/init,
legacy/v2 command and shell routes, v2 prompt admission, active provider/model compatibility,
permission/question queue, project copy, project metadata/init/update, MCP
status, v2 filesystem compatibility, PTY shells/list discovery, legacy
session todo compatibility, and queued TUI HTTP route increments.

## Status Summary

yaca is not yet a complete OpenCode superset. The native coding-agent substrate is
substantially implemented, especially tools, permissions, sessions, compaction,
plugins, MCP tools, and headless integration modes. The largest remaining gaps are
OpenCode-compatible HTTP/API coverage, exact session lifecycle controls,
provider/auth breadth, TUI feature parity, PTY/workspace/sync surfaces, and ACP.

## Confirmed Implemented Or Native Equivalent

| Area | yaca status | Evidence |
| --- | --- | --- |
| Core turn loop | Implemented | `SessionEngine::run_turn` supports multi-round tool calls, cancellation, compaction, provider streaming, and event projection. |
| Direct shell execution | Implemented | `SessionEngine::run_shell`, native `POST /sessions/:id/shell`, and OpenCode legacy/v2 session shell routes execute the real shell tool and record a synthetic user message plus assistant tool result. |
| Run status and abort | Partial | Server routes now maintain per-session run tokens, expose OpenCode-shaped `GET /session/status`, and support `POST /session/:sessionID/abort`; shell cancellation kills the spawned Unix process group. |
| Core tools | Mostly implemented | Builtins include `invalid`, `read`, `write`, `edit`, `ls`, `glob`, `find`, `grep`, `shell`, `question`, `lsp`, `skill`, `task`, plus `apply_patch`, `webfetch`, `websearch`, `todowrite`, and `plan_exit` aliases matching OpenCode names. |
| Permission system | Native superset | yaca has explicit `PermissionPlane`, rules, child-session derivation, TUI approval, headless scoped/read-only/yolo policies, and OpenCode plugin permission hook mapping. |
| Project context | Implemented | CLI discovers `AGENTS.md`, builds an environment/context system prompt, and includes available skills. |
| Skills | Implemented substrate | `.yaca/skills` and `~/.config/yaca/skills` discovery plus `skill` tool loading are present; v2 `/api/skill` returns OpenCode-shaped location-wrapped skill metadata for local skills. |
| Compaction | Native partial | Engine compaction and `ModelSummarizer` exist, with env-tunable thresholds. Legacy `POST /session/:id/summarize` now writes a persisted yaca-native system summary, but does not yet create OpenCode compaction parts or summary-backed diffs. |
| Providers | Partial | Native OpenAI-compatible, Anthropic, and Google API-key providers exist; v2 `/api/provider`, `/api/provider/:providerID`, and `/api/model` expose the active server model in OpenCode response shapes. |
| Plugin host | Partial | Native plugin protocol and bundled OpenCode adapter cover server hooks, plugin tools, chat params/messages transforms, command/message/text hooks, events, shell env, and permissions. |
| MCP tools | Partial | yaca has MCP manager/bridge and can expose MCP tools through the tool registry; `GET /mcp` reports OpenCode-shaped connected/disabled/failed status for configured MCP servers. |
| Persistence/resume | Partial | SQLite event store, session list/resume, event replay, event-log-backed session deletion, and OpenCode-shaped session info, children, message list, single-message, and basic paginated message reads are present. |
| OpenCode session API | Partial | yaca accepts prefixed `ses_...` and `msg_...` IDs and returns OpenCode-style session info, children, todo lists, `roots`/`start`/`search`/`limit` legacy session list filtering, `[{ info, parts }]` message lists, single-message reads, `limit`/`before` pagination headers, legacy diff, legacy summarize, legacy fork, legacy and v2 title/metadata/permission/archive update/delete/init plus command/shell/share routes, legacy permission respond, message deletion, part deletion, text/reasoning/tool part update, v2 `/api/session` list/create/get/context wrappers, v2 prompt admission, v2 `POST /api/session/:sessionID/agent` plus `/model` switching endpoints, and v2 compact/wait routes with current unavailable semantics. |
| OpenCode prompt async API | Partial | `POST /session/:sessionID/prompt_async` returns OpenCode-style no-content immediately, runs the turn in a background task, and uses the run registry for busy status and abort. |
| OpenCode file API | Partial | yaca exposes OpenCode-shaped `/file`, `/file/content`, `/find`, `/find/file`, `/find/symbol`, `/file/status`, plus v2 `/api/fs/read/*`, `/api/fs/list`, and `/api/fs/find` routes over the server workdir. File listing marks entries ignored by root `.gitignore`/`.ignore`; binary content detects common image/PDF MIME types; v2 fs returns location-wrapped `FileSystemEntry` data and raw file bytes; symbol and legacy status match OpenCode's current empty handler behavior. |
| OpenCode instance metadata API | Partial | yaca exposes legacy `/path`, `/agent`, `/command`, `/skill`, `/lsp`, `/formatter`, `/instance/dispose`, `/vcs`, `/vcs/status`, `/vcs/diff`, `/vcs/diff/raw`, `/vcs/apply`, plus v2 `/api/health`, `/api/location`, `/api/agent`, `/api/command`, `/api/skill`, `/api/event`, `/api/reference`, read-only `/api/integration` discovery, and OpenCode TUI publish/direct/control queue routes. LSP/formatter are empty status arrays for now. |
| Branching | Partial | Session parent/child support and branch/resume exist, but OpenCode's full session tree HTTP surface is incomplete. |
| Integration modes | Implemented native | `exec --json` and `yaca rpc` JSONL mode exist. |
| TUI base | Partial | Ratatui app has opencode-dark theme, session picker, permission/question overlays, slash commands, model switching, and render tests. |

## Confirmed Missing Or Incomplete

| OpenCode area | Missing yaca parity |
| --- | --- |
| HTTP API compatibility | yaca now has native `/sessions/*`, an OpenCode `/session` read subset, v2 `/api/health`, `/api/session` list/create/get/switch/context/prompt coverage, v2 `/api/location` plus `/api/agent`/`/api/command`/`/api/skill`, v2 `/api/fs/read/*`/`/api/fs/list`/`/api/fs/find`, `/api/event` SSE, `/api/reference`, read-only `/api/integration`, active v2 `/api/provider` plus `/api/model` coverage, OpenCode-compatible pending `/api/permission` plus `/api/question` list/reply/reject queues, `GET /mcp` status, PTY shell discovery plus empty PTY session listing, TUI publish/direct/control queue routes, basic `/project` list/current/directories/update plus `/project/git/init` routes, legacy session fork/share/unshare, and git worktree-backed `/experimental/project/:projectID/copy` create/remove/refresh plus generate-name support. OpenCode also exposes credential mutation, integration connect/auth flows, richer TUI/sync/experimental/workspace surfaces, and global control routes. |
| Session lifecycle API | Missing exact OpenCode endpoints for scoped directory/project/workspace listing and revert/unrevert. Legacy session list `roots`/`start`/`search`/`limit` filters are backed by the event log's parent/title/update-time data. Title, metadata, permission, archived-time updates, deletes, init, command routes, shell routes, legacy permission responses, local share/unshare state, metadata/message-copy fork, message deletion, part deletion, yaca-native text/reasoning/tool part update, yaca-native persisted system-message summarize, and legacy diff's empty no-summary response are backed by yaca's event log/engine or pending permission queue. Compact/wait routes currently match OpenCode's unavailable status but not future execution semantics. Todo reads are backed by yaca's native `todowrite` plane but are not durable across process restart yet. Message and v2 session pagination exist but use yaca-owned cursor formats rather than OpenCode's timestamp-anchor cursors; summarize does not yet honor OpenCode's requested provider/model with a separate compaction runner; v2 context omits agent/model switch pseudo-messages and full tool provider metadata; switched model variants are not preserved separately from provider/id yet. Full OpenCode part parity is still incomplete for file, subtask, step, snapshot, patch, agent, retry, compaction variants and exact tool-state response fields, so fork and summarize cannot yet reproduce those OpenCode-only part variants; share/unshare uses a local `yaca://` URL rather than OpenCode's remote share service. |
| Abort/control | Basic busy status, abort, prompt_async, and v2 prompt admission exist. Missing OpenCode's richer runner behavior for background jobs, durable queue promotion, retry statuses, child job cancellation, and event publication for async failures/status changes. |
| Event stream | `/api/event` sends OpenCode-shaped SSE with `server.connected` and yaca envelope payloads. Missing OpenCode's full native event type taxonomy, durable event replay, and location-aware filtering for every event source. |
| Revert/snapshot/diff | OpenCode has snapshot-backed `revert`, `unrevert`, compaction-part summarize, and message diff APIs. yaca exposes `GET /session/:id/diff` with the current no-summary empty response and `POST /session/:id/summarize` with a yaca-native persisted system summary, but has no equivalent snapshot/revert service, OpenCode compaction part, or persisted message summary diffs yet. |
| File HTTP routes | Basic OpenCode legacy file HTTP routes and v2 filesystem read/list/find routes are present, including root `.gitignore`/`.ignore` ignored flags for legacy listing, common image/PDF MIME sniffing for binary content, location-wrapped v2 entries, and raw v2 file reads. Remaining gaps are exact filesystem service ranking/glob semantics, full mime-types coverage, location query/workspace routing, nested ignore file parity, and real LSP-backed symbol search when available. |
| Instance routes | Basic path, agent, command, skill, lsp, formatter, dispose, VCS info, status, diff, raw diff, apply, and v2 location/agent/command/skill endpoints are present. Remaining gaps are exact branch-mode diff parity, OpenCode patch byte caps/empty patch behavior, real LSP/formatter status integration, and full command/agent/skill config/plugin merging. |
| Config/catalog routes | Active provider/model v2 routes exist, but yaca has not moved the full resolved config/model catalog into server state. Missing full OpenCode config/catalog metadata and update semantics; yaca reads its own `~/.config/yaca/config.yaml`. |
| Provider/auth breadth | Missing AI SDK provider breadth, full models.dev metadata/autoload, provider status/cost/limit metadata, OAuth authorize/callback flows, provider auth methods, credential mutation, integration connect/auth flows, and Console/org switching. |
| PTY | `/api/pty/shells` discovers local shells and `/api/pty` reports no active yaca-managed PTY sessions. Missing real PTY creation/update/removal, retained PTY state, websocket connect tokens, and websocket attach flow. |
| TUI control API | `/tui/publish`, direct `/tui/append-prompt`, `/tui/open-help`, `/tui/open-sessions`, `/tui/open-themes`, `/tui/open-models`, `/tui/submit-prompt`, `/tui/clear-prompt`, `/tui/execute-command`, `/tui/show-toast`, `/tui/select-session`, and `/tui/control/next`/`response` queue routes exist. Missing real TUI main-loop integration and event-bus delivery parity. |
| TUI full feature parity | Missing or incomplete OpenCode command palette, theme picker/bundled theme library, model variant picker, skill picker error UI (`9dadc24`), rich markdown/diff/code rendering, usage/cost display wiring, prompt stash, and full keymap/leader UX. |
| MCP HTTP/auth routes | `GET /mcp` status is backed by yaca's real MCP manager and matches OpenCode's status union for connected, disabled, and failed local servers. Missing OpenCode-compatible dynamic add, OAuth start/callback/authenticate/remove, connect, and disconnect routes. |
| Permission/question HTTP queues | Pending permission/question list/reply/reject APIs are backed by yaca's native ask channels. Remaining gaps are persistent saved-permission listing/removal, OpenCode's full source/tool metadata, and typed TUI deny feedback. |
| Sync/workspace/control-plane | Basic project list/current/directories routes expose the server workdir, `/project/git/init` initializes the active workdir with git, `PATCH /project/:projectID` persists name/icon/commands for the server lifetime, project copy create/remove/refresh works for `git_worktree`, and generate-name returns OpenCode-shaped slug responses. Missing OpenCode sync replay/steal/history, experimental workspace adapters/list/status/warp, full worktree HTTP management/list/reset/start-command behavior, durable project directory registry/copy listing, model-backed name generation, durable project metadata database, and control-plane move-session APIs. |
| ACP | OpenCode ships an ACP service/CLI package. yaca has no ACP-compatible surface. |
| Account/stats/github/web/upgrade CLI | OpenCode has account, stats, GitHub, web, upgrade/uninstall, import/export, db/debug, attach, models/providers, and ACP CLI commands. yaca only covers its native exec/serve/login/sessions/tail/rpc/TUI commands. |
| OpenCode SDK client completeness for plugins | The adapter provides app log, path, project, and VCS client shims, but not the full OpenCode SDK HTTP client surface expected by every possible plugin, especially TUI/plugin UI APIs. |

## Next Implementation Candidates

1. Add richer OpenCode run-state lifecycle, including retry statuses,
   prompt_async error events, and status events.
2. Add LSP-backed `/find/symbol` results for the file HTTP API.
3. Add TUI skill picker/error handling parity from OpenCode `9dadc24`.
4. Tighten VCS branch-mode diff parity, patch byte caps, and empty patch
   behavior to match OpenCode's `project/vcs.ts` exactly.
5. Add OpenCode-compatible compaction-part summarize, revert, unrevert, and
   remaining OpenCode-only session lifecycle details.

Each candidate should be implemented with a red test first, verified with
`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace`, then committed atomically.
