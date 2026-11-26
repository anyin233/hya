# Session Engine, Subagents & Channels

`crates/hya-engine` builds full-mode interactive, headless, multi-agent, and Workflow execution on top of the `hya-core` kernel.

---

## 1. Turn Lifecycle & Immutable `TurnBinding`

When a prompt, slash command, or shell turn enters `SessionEngine`:

1. **Lease reservation:** Each session allows at most one active turn at a time (`TurnGate` / `TurnLease`). Concurrent requests receive `409 session_busy`.
2. **Immutable `TurnBinding`:** At turn start, the engine resolves the session's `CatalogScope` (`Global`, `Directory`, or `Project`) into an immutable snapshot of tools, skills, hooks, agent definitions, and guidance. If a user installs a bundle or reloads skills mid-turn, the running turn finishes cleanly on its captured binding while the *next* turn picks up the new generation.
3. **File snapshots & `/undo`:** Before mutating tools run, workspace file hashes/blobs are recorded (`FilesChanged` + `file_blob`). Calling `RevertSession` (`/undo`) hides the reverted messages in the projection and restores prior bytes on disk until `/redo` or the next user message commits the cut.

---

## 2. Episodic Resident Subagents (`ResidentSupervisor`)

Since `0.41.0` ([ADR-0015](../adr/0015-unified-resident-subagent-lifecycle.md)), Hya has **one unified subagent model**: every child spawned via `task` (or a Workflow Stage) is an **episodic resident actor** (`MAX_SUBAGENT_DEPTH = 2`).

```mermaid
stateDiagram-v2
    [*] --> ActiveEpisode: task() spawns child<br/>Mints handle + #DM + ActorClaim(epoch=1)
    ActiveEpisode --> Archived: Calls report() (or parent archive())<br/>Commits state Handoff + releases claim & budget
    Archived --> ActiveEpisode: Parent sends mail on #DM<br/>AgentRestarted(epoch+1) with system prompt + prior Handoff + DM
```

### Key invariants

- **Deterministic handles:** Minted on the team-root log as `main/<type>-<operator>` (e.g. `main/hya-scout-suzuran`).
- **Transactional epoch fencing (`ActorClaim`):** Every durable write by a subagent checks its `(SessionId, ActorEpoch, OwnerRunId)` claim inside SQLite. A stale or superseded worker can never commit late tool results or consume mail meant for its replacement.
- **Zero-cost sleep & Handoff revival:** On `report`, Hya checks that the subagent has no unread mail and no still-running children, summarizes its episode into a state-only **Handoff** (`model` summarizer or `deterministic` projection summary), refunds its concurrency/capacity lease, and archives it. Later downward mail on its `#DM` revives the same handle and session with `system prompt + handoff(n) + new DM` instead of replaying a huge stale transcript.
- **Concurrency & runaway budgets (`SubagentGovernor`):** Bounds concurrent streaming members (`max_concurrency`), live team members (`per_run_budget`), total team turns (`per_team_turn_budget`), and total mail events (`per_team_message_budget`).

---

## 3. Strictly Vertical Channel Plane

Inter-agent mail ([ADR-0016](../adr/0016-channel-communication-plane.md)) is event-sourced on the **team-root** session's `event_log` (`ChannelCreated`, `MailSent`, `MailConsumed`) with **strictly vertical addressing**:

- **`#DM-{8}`:** Private 1-on-1 pipe between a parent and one direct child, persisted across archive/revive episodes.
- **`#announce-{8}`:** Broadcast pipe owned by a unit leader (`main` or a depth-1 parent) to all of its live direct children. Siblings cannot discover or message each other sideways.
- **Exactly-once cursor (`MailConsumed`):** Shared across in-turn `[NEW MAIL]` steering notices, `wait` wakes, and `read channel://...` calls, preventing duplicate synthesis turns (`TEAM QUIESCED`) when a parent already read its children's reports during its turn.

---

## 4. Governed Execution (Goal, Loop & Workflows)

- **Goal (`hya -p`) & Loop (`hya loop`) drivers:** Run iterative turns on `IterationDriver` where stop authority belongs strictly to the engine's budgets, stall fingerprint detector, `--until`/`--while` shell exit codes, and independent evaluator/verifier sessions or hooks.
- **Durable Workflow executor:** Runs compiled `hya_workflow::CompiledWorkflow` Stage DAGs on the exact same resident subagent substrate—mapping anonymous Stages to one-shot episodes, `actor:` keys to persistent resident sessions woken by mail, and `mode: loop` Stages to fresh verifier sessions.
- **Sidecar wire contract:** Per-activation JavaScript sidecars and bundle processes speak newline-delimited JSON-RPC 2.0 using plugin protocol version 1 (`initialize` retains existing `protocol_version` and `host` fields, and the only activation-specific metadata is `{ activation_id, lifecycle }`).

---

## 5. Ordered Shutdown & Crash Recovery

`hya_app::BuiltSessionEngine::shutdown` and `hya_server::ServerRuntime::drain` tear down services in strict order so no SQLite writes or orphan child processes leak:

1. Fence new turn/Workflow admissions and close open SSE/gRPC streams with `serverStopping { reason }`.
2. Cancel in-flight root and subagent turns (5s `DRAIN_DEADLINE`), recording `pending_resume` rows on graceful server stops.
3. Join `ResidentSupervisor`, mailbox/lifecycle workers, and Workflow tasks.
4. Gracefully shut down (`shutdown` RPC) and reap all MCP and plugin child processes and their tracked I/O tasks.
5. Flush queued store writes (`EXIT_DRAIN_TIMEOUT` = 30s) and release `<db>.runtime-owner.lock`.
