# OpenCode Parity Matrix

Last refreshed: 2026-06-22.

OpenCode baseline: `sst/opencode` `origin/dev`
`cd292a4ecbaeedd19239edddca77f86d9727c9ae`
(`chore: generate`; latest functional increment:
`35b3fc85d091594427a5344e2ad95128b62453b1`
`feat(core): expose session switching endpoints`).

yaca baseline: current `feat/yaca-pi-parity` branch with committed OpenCode
session, abort, file, instance metadata, VCS, prompt_async, session switching,
session context, v2 prompt admission, active provider/model compatibility, and
permission/question queue compatibility increments.

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
| Direct shell execution | Implemented | `SessionEngine::run_shell` and `POST /sessions/:id/shell` execute the real shell tool and record a synthetic user message plus assistant tool result. |
| Run status and abort | Partial | Server routes now maintain per-session run tokens, expose OpenCode-shaped `GET /session/status`, and support `POST /session/:sessionID/abort`; shell cancellation kills the spawned Unix process group. |
| Core tools | Mostly implemented | Builtins include `invalid`, `read`, `write`, `edit`, `ls`, `glob`, `find`, `grep`, `shell`, `question`, `lsp`, `skill`, `task`, plus `apply_patch`, `webfetch`, `websearch`, `todowrite`, and `plan_exit` aliases matching OpenCode names. |
| Permission system | Native superset | yaca has explicit `PermissionPlane`, rules, child-session derivation, TUI approval, headless scoped/read-only/yolo policies, and OpenCode plugin permission hook mapping. |
| Project context | Implemented | CLI discovers `AGENTS.md`, builds an environment/context system prompt, and includes available skills. |
| Skills | Implemented substrate | `.yaca/skills` and `~/.config/yaca/skills` discovery plus `skill` tool loading are present; v2 `/api/skill` returns OpenCode-shaped location-wrapped skill metadata for local skills. |
| Compaction | Native partial | Engine compaction and `ModelSummarizer` exist, with env-tunable thresholds. OpenCode's explicit `/session/:id/summarize` lifecycle is still missing. |
| Providers | Partial | Native OpenAI-compatible, Anthropic, and Google API-key providers exist; v2 `/api/provider`, `/api/provider/:providerID`, and `/api/model` expose the active server model in OpenCode response shapes. |
| Plugin host | Partial | Native plugin protocol and bundled OpenCode adapter cover server hooks, plugin tools, chat params/messages transforms, command/message/text hooks, events, shell env, and permissions. |
| MCP tools | Partial | yaca has MCP manager/bridge and can expose MCP tools through the tool registry. |
| Persistence/resume | Partial | SQLite event store, session list/resume, event replay, and OpenCode-shaped session info, children, message list, single-message, and basic paginated message reads are present. |
| OpenCode session API | Partial | yaca accepts prefixed `ses_...` and `msg_...` IDs and returns OpenCode-style session info, children, `[{ info, parts }]` message lists, single-message reads, `limit`/`before` pagination headers, v2 `/api/session` list/create/get/context wrappers, v2 prompt admission, v2 `POST /api/session/:sessionID/agent` plus `/model` switching endpoints, and v2 compact/wait routes with current unavailable semantics. |
| OpenCode prompt async API | Partial | `POST /session/:sessionID/prompt_async` returns OpenCode-style no-content immediately, runs the turn in a background task, and uses the run registry for busy status and abort. |
| OpenCode file API | Partial | yaca exposes OpenCode-shaped `/file`, `/file/content`, `/find`, `/find/file`, `/find/symbol`, and `/file/status` routes over the server workdir. File listing marks entries ignored by root `.gitignore`/`.ignore`; binary content detects common image/PDF MIME types; symbol and status match OpenCode's current empty handler behavior. |
| OpenCode instance metadata API | Partial | yaca exposes legacy `/path`, `/agent`, `/command`, `/skill`, `/lsp`, `/formatter`, `/instance/dispose`, `/vcs`, `/vcs/status`, `/vcs/diff`, `/vcs/diff/raw`, `/vcs/apply`, plus v2 `/api/health`, `/api/location`, `/api/agent`, `/api/command`, `/api/skill`, `/api/event`, `/api/reference`, and read-only `/api/integration` discovery. LSP/formatter are empty status arrays for now. |
| Branching | Partial | Session parent/child support and branch/resume exist, but OpenCode's full session tree HTTP surface is incomplete. |
| Integration modes | Implemented native | `exec --json` and `yaca rpc` JSONL mode exist. |
| TUI base | Partial | Ratatui app has opencode-dark theme, session picker, permission/question overlays, slash commands, model switching, and render tests. |

## Confirmed Missing Or Incomplete

| OpenCode area | Missing yaca parity |
| --- | --- |
| HTTP API compatibility | yaca now has native `/sessions/*`, an OpenCode `/session` read subset, v2 `/api/health`, `/api/session` list/create/get/switch/context/prompt coverage, v2 `/api/location` plus `/api/agent`/`/api/command`/`/api/skill`, `/api/event` SSE, `/api/reference`, read-only `/api/integration`, active v2 `/api/provider` plus `/api/model` coverage, OpenCode-compatible pending `/api/permission` plus `/api/question` list/reply/reject queues, and basic git worktree-backed `/experimental/project/:projectID/copy` create/remove/refresh support. OpenCode also exposes `/api/mcp`, `/api/pty`, credential mutation, integration connect/auth flows, legacy file/session routes, TUI/control/sync/experimental/workspace surfaces, and global control routes. |
| Session lifecycle API | Missing exact OpenCode endpoints for scoped directory/project/workspace listing, todo, diff, update title/permissions/archive, delete, fork raw payload, init, share/unshare, summarize, revert/unrevert, permission respond, message deletion, part deletion, and part update. Compact/wait routes currently match OpenCode's unavailable status but not future execution semantics. Message and v2 session pagination exist but use yaca-owned cursor formats rather than OpenCode's timestamp-anchor cursors; v2 context omits agent/model switch pseudo-messages and full tool provider metadata; switched model variants are not preserved separately from provider/id yet. |
| Abort/control | Basic busy status, abort, prompt_async, and v2 prompt admission exist. Missing OpenCode's richer runner behavior for background jobs, durable queue promotion, retry statuses, child job cancellation, and event publication for async failures/status changes. |
| Event stream | `/api/event` sends OpenCode-shaped SSE with `server.connected` and yaca envelope payloads. Missing OpenCode's full native event type taxonomy, durable event replay, and location-aware filtering for every event source. |
| Revert/snapshot/diff | OpenCode has snapshot-backed `revert`, `unrevert`, and message diff APIs. yaca has tool outputs and event projection, but no equivalent snapshot/revert service. |
| File HTTP routes | Basic OpenCode legacy file HTTP routes are present, including root `.gitignore`/`.ignore` ignored flags for listing and common image/PDF MIME sniffing for binary content. Remaining gaps are exact filesystem service semantics, nested ignore file parity, and real LSP-backed symbol search when available. |
| Instance routes | Basic path, agent, command, skill, lsp, formatter, dispose, VCS info, status, diff, raw diff, apply, and v2 location/agent/command/skill endpoints are present. Remaining gaps are exact branch-mode diff parity, OpenCode patch byte caps/empty patch behavior, real LSP/formatter status integration, and full command/agent/skill config/plugin merging. |
| Config/catalog routes | Active provider/model v2 routes exist, but yaca has not moved the full resolved config/model catalog into server state. Missing full OpenCode config/catalog metadata and update semantics; yaca reads its own `~/.config/yaca/config.yaml`. |
| Provider/auth breadth | Missing AI SDK provider breadth, full models.dev metadata/autoload, provider status/cost/limit metadata, OAuth authorize/callback flows, provider auth methods, credential mutation, integration connect/auth flows, and Console/org switching. |
| PTY | Missing OpenCode PTY management and websocket connect-token flow. |
| TUI control API | Missing `/tui/*` server control/event queue endpoints for append/open/help/models/themes/submit/clear/execute/toast/select/control. |
| TUI full feature parity | Missing or incomplete OpenCode command palette, theme picker/bundled theme library, model variant picker, skill picker error UI (`9dadc24`), rich markdown/diff/code rendering, usage/cost display wiring, prompt stash, and full keymap/leader UX. |
| MCP HTTP/auth routes | yaca has MCP execution substrate but not OpenCode-compatible MCP status/add/auth/connect/disconnect HTTP routes. |
| Permission/question HTTP queues | Pending permission/question list/reply/reject APIs are backed by yaca's native ask channels. Remaining gaps are persistent saved-permission listing/removal, OpenCode's full source/tool metadata, and typed TUI deny feedback. |
| Sync/workspace/control-plane | Basic v2 project copy create/remove/refresh works for `git_worktree`. Missing OpenCode sync replay/steal/history, experimental workspace adapters/list/status/warp, full worktree HTTP management/list/reset/start-command behavior, project directory registry/refresh listing, project copy name generation, and control-plane move-session APIs. |
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
5. Add OpenCode-compatible todo, diff, summarize, update, delete, share, fork,
   revert, and part/message mutation session lifecycle routes.

Each candidate should be implemented with a red test first, verified with
`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace`, then committed atomically.
