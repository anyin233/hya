# hya Integration Contract Research

## Existing Frontend And Backend

The current Rust `hya` frontend starts an in-process native runtime by default
and enters `hya_tui::Tui`. HTTP mode uses `ServerMode`, which either attaches to
an explicit server or calls `hya_sdk::ServerHandle::spawn_hya_backend`.

`ServerHandle` already:

- starts `hya-backend serve --bind 127.0.0.1:0`;
- waits up to 180 seconds for the listen URL;
- drains stdout and stderr while waiting;
- creates a dedicated process group;
- terminates the group and any listener retaining the announced port on drop.

The new launcher should reuse this behavior instead of supervising the backend
again in TypeScript.

## Compat Surface

`hya-server` already exposes the SDK families needed by the imported TUI,
including path/project, config/catalog, agents, commands, sessions/messages,
prompt/shell/command actions, abort/revert/compact, files, permissions,
questions, MCP, LSP, formatter, TUI control, and `/global/event` SSE.

Existing Rust tests cover broad endpoint shapes, but they do not execute the
pinned JavaScript SDK as one TUI bootstrap flow. The migration therefore needs
one package-level integration test that uses the exact retained SDK calls
against a temporary real backend.

## Confirmed Question Event Gap

`PermissionRequests` owns a broadcast channel and emits asked/replied events.
Compat event streams merge those events with the engine bus.

`QuestionRequests` currently owns only a pending map. It inserts requests and
serves list/reply/reject routes, but has no broadcast sender, and the three
Compat SSE routes do not merge question events. An SDK-driven TUI can list a
question but cannot reliably receive its live lifecycle.

The root fix is one broadcast lifecycle in `QuestionRequests`, consumed by all
Compat event routes. It must publish after insertion and after successful
reply/reject completion to avoid races and false completion events.

## Process And Storage Scope

The default hya backend currently uses an in-memory store when no database path
is supplied. This task does not silently introduce a new persistent database
policy. The launcher can attach to a user-managed persistent hya server; a
separate persistence-default change requires its own requirement and data-risk
review.

## Distribution Constraint

The current source installer builds `hya` and `hya-backend`. Current release
automation packages only `hya`. A default `hya-ts` launch needs both the new
launcher, `hya-backend`, and the prepared TypeScript runtime, so install and
release layouts must be extended together while retaining the existing binary.
