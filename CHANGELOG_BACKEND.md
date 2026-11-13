# 0.46.0

## Breaking changes

- Databases created by earlier versions are refused with an explicit error; there is no migration. Stop the old backend and move its database away before starting this version with a fresh database.
- Reinstall installed bundles. The manifest and prepared package contract now use complete bundles and prepared format 4; old installation records are rejected. Use `kind: Bundle` and an `agents` list, even for one agent; `agent`, `api_version`, and per-agent `harness_access` are rejected. Bun extensions require an explicit default server module and single-argument hooks; Claude extensions require explicit sources. Unsupported Bun context fields `messageID`, `agent`, `directory`, `worktree`, `abort`, and `ask` are removed.
- `--db` accepts a plain filesystem path, `""`, or `":memory:"` only. Replace every `sqlite:` or `file:` URI, including in-memory URIs. Hard links and bind mounts are unsupported aliases for database ownership.
- Restart now stops and drains the old backend, then starts a fresh daemon, with a short listener gap. Interrupted root sessions resume through a user-role restore prompt; unanswered permissions and questions are cancelled, never treated as successful tools. Completed results remain, but interrupted tools may already have had side effects. `serve check`, `restart --verify`, and `restart --exe` are removed.
- Remove uses of `hya rpc`, the `run` alias, `--print-logs`, `--log-level`, and `serve --hostname/--port/--mdns/--cors`. `hya update` uses the release installer; the signed-metadata updater is removed. The unavailable `DisposeProcess`, `UpgradeProcess`, `StartOauth`, and `CompleteOauth` RPCs are removed.
- Use `permission.model` instead of `permission.mode` and lowercase `allow`/`ask`/`deny` rule effects. Remove `oauth --type` aliases and the no-op `--device` flag. Legacy `models.yml.cache` import, UUID session IDs, retired raw event formats, category prompt/token shaping, and replay of legacy `todowrite` results are removed. Malformed auth YAML now reports an error.
- MCP calls stay in their originating turn; automatic backgrounding and reclaim turns are removed. Established provider stream failures surface without replay, router errors return directly, and error prose alone no longer makes a failure retryable.
- Workflow successors of a failed stage are skipped under every failure policy, including `collect_all`; independent branches continue. Anonymous stage actors end with their stage. Loop results distinguish stalled or invalid runs from achievement.
- Task prompts arrive as the first mail on the parent DM. Agent handle mail aliases resolve to persistent DM channels. Direct client turns on live member sessions are refused; steer them with mail instead.
- Native bundle materialization requires POSIX `tee` on `PATH` to prevent intermittent launch failures during concurrent spawning.

## Multi-agent lifecycle

- First-party agents now emphasize completing deliverables and checks before ending a turn; team members finish assignments with `report`, and reviewers check stated requirements as well as code paths.
- Register task batches atomically and return every committed member even if initialization later fails. Initial task mail survives recovery and is published before initialization hooks; parked registration can return its identity before those hooks finish.
- Keep live-member capacity accounting across turns and restarts, including parked Workflow actors. Archive releases capacity and revival consumes it; creation hooks run once.
- Commit reports, endings, mail and claim release together. Refused reports leave a durable notice; sidecar loss reports failure while preserving completed reports and waking parents reliably.
- Await cancellation and descendant cleanup before ending actors. Root teardown sends no failure mail; work after a team kill starts with fresh team context and counters.
- Revive workers with their preflighted runtime, stage settings and bounded context. User-stopped workers retain unfinished mail and resume their existing prompt once; queued work is retained through recovery and retirement.
- Resume recovered workers on every runtime entry point, admitting root restores before recovered work can drive the parent. Headless runs keep one root continuation owner and include unread root mail in pending work.

## Workflows and mail

- Start ready stages and their verifiers as soon as dependencies permit, preserving evidence order. Shared actors are serialized and may serve stages that were previously rejected as same-level conflicts.
- Preserve truthful stage failures, skipped-stage records and loop wrap-up errors. Cancellation owns cleanup and terminal records even after the caller disconnects; Workflow reads remain responsive during long runs.
- Authorize mail and acknowledge delivery in the same transaction as its result. Failed delivery leaves mail unread; running-turn notices preserve non-object tool results. Outgoing DM history copies no longer suppress an owed parent synthesis.
- Keep handoff boundaries and base prompts on revival. Manual compaction and handoff serialize with turns and retain tool evidence; compaction records commit atomically and persistence errors fail the turn.

## Runtime and CLI

- Keep partial text, reasoning, file changes and completed tool results when a turn is cancelled, so replay and undo remain complete.
- Use one database ownership lock for concurrent daemon starters; canonical relative, absolute and symlink paths share ownership. Runtime rebuilds receive distinct ownership and claim-fenced writes prevent stale actors from consuming mail or recording usage.
- Cancel turns before closing interaction bridges, then join runtime, server, plugin and MCP tasks during shutdown. A turn reserved before shutdown is refused with a stopping error.
- `hya update` refreshes changed bundles or restarts a changed backend; connected TUIs reconnect after restart. `hya sessions` lists root sessions newest first; `--all` includes children. Workflow list/info read the catalog without starting an engine.
- `exec --time-budget` supplies staged deadline notices and caps Bash/wait timeouts. Unfinished headless runs receive bounded continuation notices; recovery recognizes leaked DSML/XML tool calls and spurious internal summaries while respecting code examples and provider-step boundaries.
- Fix Bash hangs and false timeouts when detached daemons retain pipe or PTY output descriptors. OAuth binds its loopback callback before opening the browser; entropy and other boundary failures surface explicitly.

## Bundles, plugins and providers

- Refresh bundles explicitly with `hya bundle refresh`; unchanged bindings reuse prepared bundles. Install, remove, enable and configuration changes refresh running backends without a daemon restart. Failed sources retain their own previous contributions without blocking healthy sources.
- Preserve unreadable bundle rows and disabled state. A module's exports can serve distinct agent views through one extension process; validate complete declarations, including unselected exports. Changes to process declarations invalidate Workflow revisions.
- Synchronize hook contracts across Rust and TypeScript, allow concurrent adapter calls, require successful initialization and identify extensions returning invalid hook results. MCP notifications keep their connection; stdio frames are bounded.
- Make providers added or refreshed at runtime immediately available to later spawns and Workflows. Concurrent configuration writes no longer overwrite each other; project model and effort apply together.
- Trust native TLS roots, including `SSL_CERT_FILE`, alongside bundled web roots. Reject oversized final requests before dispatch, including smaller fallback models. Usage comes from the event-sourced projection without tokenizer downloads or duplicate ledgers.

## API and measured performance

- Persist ordinary user messages and completed text/reasoning boundaries atomically. Session deletion removes all retained session records, claims and admissions together.
- Return intended HTTP/gRPC error statuses instead of generic 500s; bound list reads and align gRPC pagination with HTTP. Live streams exclude pre-subscription backlog and retain notices through delta bursts. SDK recovery discards incomplete tool input after stream gaps.
- Stream tool-input deltas live without persisting each fragment; completed input remains durable. The lifecycle benchmark records zero SQL statements per streamed delta after removing claim polling.
- On the 64-worker lifecycle benchmark, the six-row projection/mail copy-volume proxy fell from a median 241.9 MB to 34.4 MB across five samples per state. Mail bodies are stored once across inboxes and channel history.
- Share live-event conversion and SSE encoding across subscribers: the eight-subscriber case uses one conversion instead of eight. Prepared bundle assets are shared as well.
