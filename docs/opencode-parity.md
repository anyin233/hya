# OpenCode Parity Matrix

Last refreshed: 2026-06-22.

OpenCode baseline: `sst/opencode` `origin/dev`
`9dadc2455fff77bb461135e12e9a775c3c14c98a`
(`fix(tui): render skill load errors inline (#33298)`).

yaca baseline: `c5cfdaa` (`feat(server): run session shell commands`).

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
| Core tools | Mostly implemented | Builtins include `invalid`, `read`, `write`, `edit`, `ls`, `glob`, `find`, `grep`, `shell`, `question`, `lsp`, `skill`, `task`, plus `apply_patch`, `webfetch`, `websearch`, `todowrite`, and `plan_exit` aliases matching OpenCode names. |
| Permission system | Native superset | yaca has explicit `PermissionPlane`, rules, child-session derivation, TUI approval, headless scoped/read-only/yolo policies, and OpenCode plugin permission hook mapping. |
| Project context | Implemented | CLI discovers `AGENTS.md`, builds an environment/context system prompt, and includes available skills. |
| Skills | Implemented substrate | `.yaca/skills` and `~/.config/yaca/skills` discovery plus `skill` tool loading are present. |
| Compaction | Native partial | Engine compaction and `ModelSummarizer` exist, with env-tunable thresholds. OpenCode's explicit `/session/:id/summarize` lifecycle is still missing. |
| Providers | Partial | Native OpenAI-compatible, Anthropic, and Google API-key providers exist. |
| Plugin host | Partial | Native plugin protocol and bundled OpenCode adapter cover server hooks, plugin tools, chat params/messages transforms, command/message/text hooks, events, shell env, and permissions. |
| MCP tools | Partial | yaca has MCP manager/bridge and can expose MCP tools through the tool registry. |
| Persistence/resume | Partial | SQLite event store, session list/resume, and event replay are present. |
| Branching | Partial | Session parent/child support and branch/resume exist, but OpenCode's full session tree HTTP surface is incomplete. |
| Integration modes | Implemented native | `exec --json` and `yaca rpc` JSONL mode exist. |
| TUI base | Partial | Ratatui app has opencode-dark theme, session picker, permission/question overlays, slash commands, model switching, and render tests. |

## Confirmed Missing Or Incomplete

| OpenCode area | Missing yaca parity |
| --- | --- |
| HTTP API compatibility | yaca uses native `/sessions/*`; OpenCode exposes `/session/*`, `/config`, `/file`, `/provider`, `/permission`, `/question`, `/mcp`, `/tui`, `/sync`, `/experimental/*`, `/pty`, `/project`, `/workspace`, `/control`, and `/global` groups. |
| Session lifecycle API | Missing exact OpenCode endpoints for status, get/list by scope, children, todo, diff, paginated messages, individual message, update title/metadata/permissions/archive, delete, fork raw payload, abort, init, share/unshare, summarize, prompt_async, revert/unrevert, permission respond, message deletion, part deletion, and part update. |
| Abort/control | yaca turn cancellation tokens exist internally, but the server does not maintain per-session run handles or expose OpenCode-equivalent abort/status controls. |
| Revert/snapshot/diff | OpenCode has snapshot-backed `revert`, `unrevert`, and message diff APIs. yaca has tool outputs and event projection, but no equivalent snapshot/revert service. |
| File HTTP routes | OpenCode exposes file text search, file search, symbol search, list, content, and status routes. yaca has equivalent tool-level read/grep/glob/find behavior, but not the HTTP API surface. |
| Instance routes | Missing OpenCode-compatible path, VCS info/status/diff/raw diff/apply, command list, agent list, skill list, LSP status, and formatter status endpoints. |
| Config routes | Missing OpenCode-compatible config get/update/provider metadata routes. yaca reads its own `~/.config/yaca/config.yaml`. |
| Provider/auth breadth | Missing AI SDK provider breadth, models.dev metadata/autoload, provider status/cost/limit metadata, OAuth authorize/callback flows, provider auth methods, and Console/org switching. |
| PTY | Missing OpenCode PTY management and websocket connect-token flow. |
| TUI control API | Missing `/tui/*` server control/event queue endpoints for append/open/help/models/themes/submit/clear/execute/toast/select/control. |
| TUI full feature parity | Missing or incomplete OpenCode command palette, theme picker/bundled theme library, model variant picker, skill picker error UI (`9dadc24`), rich markdown/diff/code rendering, usage/cost display wiring, prompt stash, and full keymap/leader UX. |
| MCP HTTP/auth routes | yaca has MCP execution substrate but not OpenCode-compatible MCP status/add/auth/connect/disconnect HTTP routes. |
| Permission/question HTTP queues | yaca has native permission and interaction planes, but no OpenCode-compatible `/permission` and `/question` list/reply/reject APIs. Typed TUI deny feedback is still deferred. |
| Sync/workspace/control-plane | Missing OpenCode sync replay/steal/history, experimental workspace adapters/list/status/warp, worktree HTTP management, project copy name generation, and control-plane move-session APIs. |
| ACP | OpenCode ships an ACP service/CLI package. yaca has no ACP-compatible surface. |
| Account/stats/github/web/upgrade CLI | OpenCode has account, stats, GitHub, web, upgrade/uninstall, import/export, db/debug, attach, models/providers, and ACP CLI commands. yaca only covers its native exec/serve/login/sessions/tail/rpc/TUI commands. |
| OpenCode SDK client completeness for plugins | The adapter provides app log, path, project, and VCS client shims, but not the full OpenCode SDK HTTP client surface expected by every possible plugin, especially TUI/plugin UI APIs. |

## Next Implementation Candidates

1. Add OpenCode-compatible session read/list endpoints over the existing store:
   `GET /session`, `GET /session/:sessionID`, `GET /session/:sessionID/message`.
2. Add server-side run-state handles and `POST /session/:sessionID/abort`.
3. Add OpenCode-compatible file HTTP routes backed by existing `read`, `grep`,
   `glob`, and `find` implementations.
4. Add instance metadata routes for path, VCS, skill list, command list, and LSP
   status using existing yaca subsystems.
5. Add TUI skill picker/error handling parity from OpenCode `9dadc24`.

Each candidate should be implemented with a red test first, verified with
`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace`, then committed atomically.
