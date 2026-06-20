# Design: yaca — Rust Multi-Agent Coding Agent

> Merged from three parallel planners (architect / rust-impl / risk lenses — see
> `research/drafts/`). Conflicts reconciled in §13. Grounded in the reference
> research under `research/` (opencode, omo team mode, Claude Code `/goal`).
> Confirmed decisions D1–D4 live in `prd.md`.

## 1. Architecture overview

yaca is **one workspace, two binaries**: a `core` **server** (owns all agent
logic, state, providers, orchestration, goal evaluation) and a **ratatui TUI
client** that talks to it over a local HTTP + SSE API (D3). The server is
provider-agnostic via a Provider/Protocol/Route layer that normalizes every LLM
into ONE canonical event stream (D2). Subagents are **child sessions** run as
**supervised async tasks inside the server**, coordinated by an in-memory
mailbox + task board behind traits (D4). A goal engine wraps the agent loop with
an **independent transcript-only evaluator** (D4).

```
 ratatui TUI client ──HTTP/SSE──► core server
                                    ├─ SessionEngine ──► AgentLoop ──► ProviderRouter ─► {Anthropic, OpenAI, OpenAI-compat/local}
                                    │       │                              (Provider/Protocol/Route → canonical Event stream)
                                    │       ├─ EventBus (broadcast) ──► SSE fan-out + GoalEngine + Projector
                                    │       ├─ ToolRegistry ──► PermissionPlane
                                    │       └─ SessionStore (SQLite: event log + projection)
                                    ├─ TeamOrchestrator ──► Mailbox + TaskBoard (trait; in-memory v0) ──► supervised member tasks
                                    │                          └─ WorktreeManager (git) + TmuxPaneManager (tmux)  [observability]
                                    └─ GoalEngine ──► independent cheap-model evaluator (NO tools; reads TranscriptProjection only)
```

**Single multi-threaded tokio runtime.** Members are `tokio::spawn` tasks, not
OS processes (v0); git/tmux are reached by shelling out. The mailbox/task board,
provider router, and store all sit behind traits so a file-backed multi-process
substrate and a remote SDK can be added in v1 without touching the agent loop.

## 2. Crate layout

9 crates; acyclic graph; `yaca-proto` is dependency-light and shared by client +
server so the TUI never compiles sqlx/tokio-heavy code it doesn't need.

```
yaca-proto     wire types: ids, Message, Part, Event, Envelope, ToolSchema. (serde/uuid/time only)
yaca-provider  Provider/Protocol/Route traits + Anthropic/OpenAI/OpenAI-compat impls + FakeProvider
yaca-tool      Tool trait, JSON-schema, ToolRegistry, PermissionPlane
yaca-store     sqlx SQLite: event log + projection + replay
yaca-core      domain: SessionEngine, AgentLoop, TeamOrchestrator (module), GoalEngine (module), EventBus
yaca-server    axum HTTP/SSE server over yaca-core
yaca-client    thin SDK: typed HTTP/SSE client (shared by TUI; future remote tooling)   [architect's wire-stability point]
yaca-tui       ratatui client over yaca-client
yaca-cli       `yaca` umbrella binary: spawns server + TUI; `serve`, `-p` headless, `tail-session`
```

Dependency direction: `cli → {server, tui}`; `server → core`; `tui → client → proto`;
`core → {provider, tool, store, proto}`; everything → `proto`. `yaca-core` is
HTTP-agnostic (testable without a server). team + goal are **modules inside
`yaca-core`** (not separate crates) because both are tightly coupled to
`SessionEngine` and the `EventBus`; splitting them would invert the dependency.

Pinned stack: tokio, tokio-util (`CancellationToken`), axum + tower-http, hyper,
reqwest + eventsource-stream, serde/serde_json, schemars + jsonschema, sqlx
(sqlite, WAL), thiserror (libs) / anyhow (bins only), uuid v7, time, tracing,
ratatui + crossterm, clap, secrecy (`SecretString` for keys). `git` and `tmux`
are external binaries (shell-out) — no `git2`/libgit2 (cross-compile pain).

## 3. Core contracts (`yaca-proto`)

Two invariants: **tagged enums everywhere** (`#[serde(tag=...)]`, never
`untagged` — the TUI must discriminate without trial-parsing) and **newtype every
id** (`SessionId`, `MessageId`, `PartId`, `ToolCallId`, `TeamRunId`, `MemberId`,
`GoalId`, …; `EventSeq(u64)` monotonic per session).

- **`Message`** — tagged union by role: `User | Assistant | System | Synthetic |
  AgentSwitched | ModelSwitched | Compaction`. Assistant carries `agent, model,
  parts[], finish, cost, tokens, time`.
- **`Part`** — `Text | Reasoning | Tool{call_id, name, state}`. `ToolPartState`:
  `Pending → Running → Completed | Error` (drives TUI rendering).
- **`Event`** — the canonical streaming event, the single most-replicated type:
  session lifecycle, message lifecycle, step start/finish, text/reasoning
  start·delta·end, tool input start·delta·end + call·result·error, permission
  asked/resolved, team lifecycle (created/member-spawned/mail/task/shutdown/
  deleted), goal set/evaluated/cleared, loop set/iteration-started/iteration-
  completed/verifier-judged/planner-planned/paused/resumed/stopped, error. Every
  event ships as `Envelope { seq: u64, ts, event }` for ordered replay across SSE
  reconnects. (Loop events carry `*_chars` counts, not raw text; full bodies live
  in `loop_iteration` rows for the TUI detail pane.)

## 4. Provider abstraction (`yaca-provider`) — the keystone

opencode's split, in Rust. **Every provider normalizes to the canonical `Event`
stream before the agent loop sees anything.** This is non-negotiable (all three
planners independently flagged it as risk #1).

- **`Provider`** — vendor facade: id, `list_models()`, `protocol_for(model)`,
  `route_for(model)`.
- **`Protocol`** — wire family (`openai_chat`, `openai_responses`,
  `anthropic_messages`): `encode(canonical req)→provider JSON` +
  `decode(native SSE)→canonical Event stream`.
- **`Route`** — endpoint + auth + transport: `complete(req, CancellationToken)
  → EventStream`. e.g. `AnthropicMessagesRoute` (x-api-key) vs `OpenAIChatRoute`
  (bearer) vs `OpenAICompatibleRoute` (Ollama/vLLM, user base_url) reuse the
  OpenAI-chat protocol.
- **`ProviderRouter::stream(model, req, cancel)`** is the single entry point used
  by the lead loop, every team member, AND the goal evaluator. **Model tiering is
  "pass a different `ModelRef`"** — that is the whole mechanism behind cheap
  goal-gates and per-category routing.
- **Capability matrix per route** (`streaming_tool_calls`, `parallel_tool_calls`,
  `usage_reporting`, `json_output`, `reasoning_stream`, `max_context`): route
  selection **rejects incompatible tasks before a turn starts** (risk #12 —
  "OpenAI-compatible ≠ OpenAI-equivalent"; a local model lacking tool-calls must
  fail preflight, not deep in the loop).
- **Auth** via `secrecy::SecretString`, lives ONLY in provider impls, never in
  `yaca-core`, never logged. Config `${env:NAME}` expansion.

## 5. Tool system + permissions (`yaca-tool`)

Two layers (opencode §4). **Canonical** `ToolSchema` (name + description +
JSON-schema input/output, generated by `schemars`). **Typed runtime wrapper**:
the `Tool` trait + `ToolCtx { session, message, call, agent, permission, events,
cancel, workdir }`. Ordered execution path, no exceptions: **decode input →
permission check → execute → validate output → emit events → project to
transcript.** Side effects NEVER begin before permission resolves (risk #9).

v0 built-in set (D1 — deliberately small): `read, write, edit, glob, grep,
shell`, the 12 `team_*` tools, `goal_*` (set/status/clear), and `loop_*`
(set/status/clear/pause/resume). No webfetch/MCP/plugins in v0.

**Permission plane** (opencode's control plane): `(Action, Resource)` evaluated
against a merged, **last-rule-wins** ruleset; default `ask`. `assert()` →
allow / deny / **pending request** (a `PermissionAsked` event the TUI surfaces;
reply via a oneshot channel: `AllowOnce | AllowAlways | Reject`). Tools call
`ctx.permission.assert(action, resource)` with **semantic scopes** (path globs,
`git *`, subagent names, external dirs). Child/member sessions **derive**
permissions from the parent (narrow-only) and are **question-denied** by
construction (a background member cannot block forever waiting on a user prompt —
risk #4). Per-turn permission **snapshot** is immutable; `AllowAlways` affects
only later turns.

## 6. Sessions & persistence (`yaca-store`) — event-sourced + projected

Append-only **event log** + **synchronous in-transaction projection**. A reader
after `append_event` always sees consistent state; rebuild = replay the log
through `project`. Parent/child `session.parent_id` is the **session tree** that
subagents live in. Canonical migration `0001_init.sql` (the contract; implement.md
Phase 1 builds exactly this):

```sql
CREATE TABLE session (
  id BLOB PRIMARY KEY, parent_id BLOB, agent TEXT NOT NULL, model TEXT NOT NULL,
  workdir TEXT NOT NULL, title TEXT, permission TEXT NOT NULL,            -- snapshot JSON
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  FOREIGN KEY(parent_id) REFERENCES session(id));
CREATE INDEX session_parent ON session(parent_id);
CREATE TABLE message (
  id BLOB PRIMARY KEY, session_id BLOB NOT NULL REFERENCES session(id) ON DELETE CASCADE,
  role TEXT NOT NULL, agent TEXT, model TEXT, finish TEXT, cost_json TEXT,
  tokens_json TEXT, created_at INTEGER NOT NULL);
CREATE INDEX message_session ON message(session_id);
CREATE TABLE part (
  id BLOB PRIMARY KEY, message_id BLOB NOT NULL REFERENCES message(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL, kind TEXT NOT NULL,                              -- text|reasoning|tool
  body_json TEXT NOT NULL, UNIQUE(message_id, seq));
CREATE TABLE event_log (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, session_id BLOB NOT NULL,
  payload TEXT NOT NULL, ts INTEGER NOT NULL);                          -- canonical Event JSON
CREATE INDEX event_log_session ON event_log(session_id);
CREATE TABLE team_run (
  id BLOB PRIMARY KEY, lead_session BLOB NOT NULL REFERENCES session(id),
  spec_json TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE team_member (
  id BLOB PRIMARY KEY, team_id BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
  session_id BLOB NOT NULL REFERENCES session(id), background_task_id TEXT,
  role TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE mail (
  id BLOB PRIMARY KEY, team_id BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
  from_ep TEXT NOT NULL, to_ep TEXT NOT NULL, kind TEXT NOT NULL, body_json TEXT NOT NULL,
  delivered_at INTEGER, acked_at INTEGER, created_at INTEGER NOT NULL);
CREATE TABLE task_board (
  id BLOB PRIMARY KEY, team_id BLOB NOT NULL REFERENCES team_run(id) ON DELETE CASCADE,
  title TEXT NOT NULL, body TEXT NOT NULL, status TEXT NOT NULL, assignee TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE goal (
  id BLOB PRIMARY KEY, session_id BLOB NOT NULL REFERENCES session(id),
  condition TEXT NOT NULL, bound_json TEXT, state TEXT NOT NULL,
  turns_evaluated INTEGER NOT NULL, last_reason TEXT,
  started_at INTEGER NOT NULL, cleared_at INTEGER);
CREATE TABLE token_ledger (
  id BLOB PRIMARY KEY, session_id BLOB NOT NULL, turn INTEGER,           -- turn = agent-turn within a session
  provider TEXT, model TEXT, team_id BLOB, completion_run_id BLOB,
  iteration INTEGER,                                                     -- loop iteration (NULL for goal/non-loop)
  role TEXT, category TEXT,                                             -- role: worker|verifier|planner|lead|team_member
  prompt_tokens INTEGER, completion_tokens INTEGER,
  confidence TEXT NOT NULL, ts INTEGER NOT NULL);                       -- confidence: actual|estimated
```
Migration `0002_pragmas.sql` sets `journal_mode=WAL`, `busy_timeout=5000`,
`foreign_keys=ON`, `synchronous=NORMAL`. Migration `0003_loop.sql` adds the loop
tables (`completion_run_id` above references either `goal.id` or `loop_run.id`):

```sql
CREATE TABLE loop_run (
  id BLOB PRIMARY KEY, session_id BLOB NOT NULL REFERENCES session(id),
  target TEXT NOT NULL, budget INTEGER NOT NULL, stop_when_satisfied INTEGER NOT NULL,
  satisfaction_threshold INTEGER NOT NULL, state TEXT NOT NULL,         -- Running|Paused|Done|Capped|NoProgress|Cancelled|Failed
  iterations_done INTEGER NOT NULL, last_score INTEGER, last_gaps_json TEXT, last_reason TEXT,
  planner_notes TEXT NOT NULL DEFAULT '', started_at INTEGER NOT NULL, finished_at INTEGER);
CREATE INDEX loop_run_session ON loop_run(session_id);
CREATE TABLE loop_iteration (
  id BLOB PRIMARY KEY, loop_id BLOB NOT NULL REFERENCES loop_run(id) ON DELETE CASCADE,
  iter_no INTEGER NOT NULL, worker_session BLOB NOT NULL REFERENCES session(id),
  gate_phase TEXT NOT NULL,                                             -- WorkerRunning|EvidenceBuilt|Verifying|Verified|Planning|Planned|IterationComplete|Stopped
  directive TEXT NOT NULL, continuity_brief TEXT, verifier_json TEXT, planner_json TEXT,
  directive_fp TEXT, gap_fp TEXT, usage_json TEXT,
  started_at INTEGER NOT NULL, finished_at INTEGER, UNIQUE(loop_id, iter_no));
```
`gate_phase` + idempotency keys `(loop_id, iter_no, role)` make mid-loop resume
deterministic (re-run a phase at most once; reuse a persisted directive).

Concurrency (risk #6): SQLite **WAL** + a **single writer** discipline; appends
are one short txn; tool/provider I/O never holds a write txn open; `busy_timeout`
+ bounded retry. `EventBus` = `tokio::sync::broadcast`; SSE subscribers that lag
get a `resync` event carrying `last_seq` and refetch via `/events?since_seq=N`
(risk #5). **Events are the source of truth; the TUI never renders raw provider
deltas — only projected state**, so reconnect/replay is deterministic.

## 7. Agent turn loop (`yaca-core::AgentLoop`)

`run_turn`: emit `MessageStarted` → loop steps { build `CompletionRequest` from
projected history + allowed-tool schemas → `ProviderRouter::stream` → fold each
canonical event (persist + bus-publish + accumulate tool calls) → if tool calls,
dispatch them **in parallel** (`FuturesUnordered`), each through the permission
plane, append results, loop again; else finish }. `CancellationToken` reaches
both the provider HTTP body and every tool. No `unwrap` in lib paths; `Cancelled`
is a distinct typed outcome end-to-end (user ctrl-c ≠ error).

## 8. Team orchestration (`yaca-core::team`) — full omo parity (D4)

**Control plane / execution plane split** (omo's core lesson). Execution plane =
supervised member tasks (child sessions). Control plane = mailbox + task board +
team state machine.

- **`TeamOrchestrator::create`** spawns members **in parallel**; each member =
  a child session under the lead + a supervised `tokio::spawn` with a child
  `CancellationToken`, a bounded `mpsc` inbox, a `watch` state channel, and a
  `JoinHandle`. The lead session may itself be reused as the lead.
- **`MailboxBackend` + `TaskBoardBackend` traits**; v0 `InMemoryMailbox` /
  `InMemoryTaskBoard` (RwLock + a per-team broadcast for "repoll" notifications).
  Trait-backed so a file-backed multi-process substrate is a v1 swap.
- **12 `team_*` tools** (thin `Tool` impls over the orchestrator). I/O contract
  (`MailEndpoint = Lead | Member(id) | Broadcast`, broadcast lead-only;
  `MailKind` **rejects shutdown kinds** — shutdown is its own typed path):

  | tool | input | output | who |
  |---|---|---|---|
  | `team_create` | `spec: TeamSpec` (1..=8 members) | `{team, members[]}` | lead |
  | `team_delete` | `{team, force: bool}` | `()` | lead |
  | `team_shutdown_request` | `{team, target: MailEndpoint}` | `()` | lead |
  | `team_approve_shutdown` | `{team, target}` | `()` | target\|lead |
  | `team_reject_shutdown` | `{team, target, reason}` | `()` | target\|lead |
  | `team_send_message` | `{team, to, kind, body, refs[]}` | `{mail_id}` | any (broadcast=lead) |
  | `team_task_create` | `{team, title, body, assignee?}` | `{task_id}` | any |
  | `team_task_list` | `{team, filter}` | `TaskSummary[]` | any |
  | `team_task_update` | `{team, id, status?, assignee?, body?}` | `TaskItem` | any |
  | `team_task_get` | `{team, id}` | `TaskItem` | any |
  | `team_status` | `{team}` | `TeamStatusSnapshot` | any |
  | `team_list` | `{}` | `TeamRunSummary[]` | any |

  Each routes through the same canonical tool wrapper + permission plane as
  `read`/`shell`. Full struct shapes: `research/drafts/design-rust-impl.md` §6.6.
- **Team state machine** (explicit, typed transitions enforced at the command
  boundary — risk #10). States: `Creating, Active, ShutdownRequested, Draining,
  Completed, Failed, ForceDeleting, Deleted`. Member states: `Spawning, Active,
  ClosureReady, Done, Failed`. Allowed transitions:

  | from | event | to |
  |---|---|---|
  | Creating | all members spawned | Active |
  | Active | `team_shutdown_request` | ShutdownRequested |
  | ShutdownRequested | all targets approved | Draining |
  | ShutdownRequested | `team_reject_shutdown` | Active |
  | Draining | all members `Done`/`Failed` | Completed |
  | Active/Draining | member task panics/cancels | (member→Failed; run stays) |
  | Creating/Active/ShutdownRequested/Draining | `team_delete{force:true}` (incl. worker-iteration reap) | ForceDeleting |
  | Completed/Failed | `team_delete` (no active members) | Deleted |
  | ForceDeleting | bg tasks cancelled + resources cleaned | Deleted |

  Any other (from, event) pair is **rejected at the tool boundary** with a typed
  error. `team_delete{force:false}` on a run with active members → rejected.
- **Members are writers** (omo): read-only consultant agents can't be members.
  Members get question-denied permissions. **Result flow is message/task-oriented,
  NOT transcript** — a member's assistant text is NEVER copied into the lead's
  context; the lead pulls summaries via `team_status` / `team_task_get`. This is
  the token-efficiency invariant (N2).
- **Categories + skills**: a `category → model + fallback-chain + prompt-append`
  resolver (omo's `resolveCategoryExecution`); skill content injected into the
  member's system prompt. Per-category model routing is what makes delegation
  cheap (route quick work to cheap models).
- **Worktrees + tmux** behind `WorktreeManager` (shell-out `git worktree`) and
  `TmuxPaneManager` (shell-out `tmux`); both own a per-team allocation registry
  with explicit cleanup; **never infer ownership from path shape** (risk #7).
  tmux is **observability**, not control — the real driver is the tokio task.

## 9. Completion engine (`yaca-core::completion`) — goal + loop (D4, D5)

Both autonomous-completion modes share ONE driver. `IterationDriver<G:
IterationGate, X: IterationExecutor>` owns turn driving, caps, cancellation, token
ledger, projection, event emission, and persistence — with **zero `if goal/loop`
branches**. Goal and loop diverge in exactly **two seams**:

| seam | goal mode | loop mode |
|---|---|---|
| `IterationExecutor` | `LeadTurnExecutor` — reuse the lead session, run one turn | `WorkerSessionExecutor` — spawn a FRESH child session per iteration, run to completion under per-iteration caps |
| `IterationGate` | `GoalGate` — 1 cheap verifier, transcript-only | `LoopGate` — cheap verifier + strong planner, both tool-less |

Driver loop: check caps/cancel → `executor.run_iteration(directive,
continuity_brief)` → `ledger.record` → `gate.judge(outcome) → GateOutcome::{ Stop{
verdict, reason } | Continue{ verdict, next_directive } }`. **The gate decides
success; the driver decides caps.** Full Rust types: `research/drafts/loop-engine-
architect.md` + `loop-engine-risk.md`.

**Authority model (non-negotiable — prevents a self-ratifying auto-agent):**
- The **engine** owns stop authority: budget / wall-clock / token / cancellation /
  no-progress caps ALWAYS win, no model override.
- The **verifier** is the ONLY success judge (transcript-only, no tools).
- The **planner** is ONLY a next-directive generator. Its output schema has **no
  `done`/`satisfied`/`stop` field** — any such text is stored as advisory rationale
  and ignored for control flow.

### Goal mode (definitive)
`LeadTurnExecutor` + `GoalGate`. The verifier judges ONLY the surfaced transcript
(Claude Code `/goal` semantics; `GoalEvaluator` is the inner type `GoalGate`
wraps). Caps (config-overridable): `max_turns=50`, `max_wall_clock=1800 s`,
`max_tokens=2_000_000`. Malformed evaluator JSON ⇒ `met=false` AND counts toward
`max_turns`. Lifecycle: `goal_set/status/clear`, resume (baselines reset),
non-interactive `yaca -p "/goal …"`.

### Loop mode (open-ended, budget-bounded)
`WorkerSessionExecutor` + `LoopGate` (the two-agent gate). Per iteration:
1. **Worker** runs a fresh child session from the planner's directive +
   `continuity_brief` (may spawn a team). Bounded by per-iteration caps.
2. **Verifier** (cheap tier) grades ONLY `(target, this iteration's projection)` →
   `{ score 0..100, satisfied, confidence, evidence_quality (missing|claim_only|
   supported|verified), critical_gaps[], regressions[], iteration_summary ≤500c,
   progress_fingerprint, reason }`. Claims alone cannot reach `verified`.
3. **Early-stop short-circuit** (before the planner, to save the strong call):
   `stop_when_satisfied && satisfied && score≥threshold(90) && critical_gaps==[] &&
   evidence_quality≥supported` ⇒ `Stop{Satisfied}`.
4. **Planner** (strong tier) runs only when continuing; sees `(target, bounded
   sliding-window of iteration_summaries, last verdict, planner_notes)` — **never
   raw worker/child transcripts** — and emits `{ directive, continuity_brief,
   planner_notes, strategy_change: bool, change_note: String }` (NO stop/done/
   satisfied field — authority is the engine's). `strategy_change`/`change_note`
   are required to satisfy the oscillation guard below; the engine rejects a
   directive whose fingerprint repeats the last two unless `strategy_change==true`
   with a non-empty `change_note`.

**Continuity discipline (A3 — no surface grows linearly in N):** worker context =
O(1) (one iteration's turns); planner context = O(window) (summaries + rewritten
`planner_notes`, with sliding compaction + a `[compacted earlier iterations]`
marker); verifier context = O(1). This preserves N2 token efficiency across long
loops.

**No-progress / oscillation detection:** per-iteration directive/gap/evidence
**fingerprints**; `max_no_progress=3` (stop `NoProgress` when score Δ<5 and critical
gaps unchanged for 3 iters); reject a planner directive whose fingerprint repeats
the last two unless it sets `strategy_change=true`.

**Loop caps (config-overridable):** explicit budget `N` required, hard ceiling
`max_iterations=100`, per-iteration `max_turns=30` + `max_tokens=500_000`, loop
`max_wall_clock=7200 s`, total tokens inherit 2_000_000; a **cost preflight**
computes worst-case spend (N × per-iter caps + planner/verifier) and surfaces it
before a non-interactive loop starts. Lifecycle: `loop_set/status/clear/pause/
resume`, resume by persisted **gate phase** (see §6), non-interactive
`yaca -p "/loop …"`. One completion run (goal XOR loop) per session at a time.

**Team caps (shared, config-overridable):** `max_members=8` per team, `max_teams=4`
concurrent per lead, per-member inbox=64, optional per-category token budget
(exceed ⇒ pause new member turns + `team_status` warning).

**v0 rule — one autonomous driver per session tree:** a loop/goal worker MAY use
team mode, but a child session may NOT start its own goal/loop (no nested drivers).

## 10. Completion ↔ team composition (the key hazard — §13 conflict 2)

The token-efficiency invariant (members don't pollute the lead transcript)
conflicts with the transcript-only judges (goal verifier; loop verifier + planner).
If team results stay in mailbox/board state, a completion-driven team turn is
**unjudgeable** (risk #2). Resolution: a first-class **Team Evidence Envelope** —
after a team turn, a bounded **structured synthetic message** is projected into the
session transcript: `{ team_id, member_ids, task statuses, member result summaries,
commands run + exit codes, changed files, unresolved blockers, child session ids,
aggregate token usage }` — **never full child transcripts**. The judge's
`TranscriptProjection` includes the envelope, so it judges without breaking
isolation. (Conditions naming a check, e.g. "`cargo test` exits 0", are best run by
the worker itself so the exit code is in the transcript.)

For **loop mode**, the planner additionally consumes a **Loop Evidence Envelope**
(generalizes the team envelope): the bounded per-iteration record (directive,
terminal outcome, verifier-graded summary, direct evidence = commands/exit-codes/
changed-files, nested Team Evidence Envelope, ledger slice, progress fingerprint).
The planner reads these structured envelopes — **never raw worker/child
transcripts**. If evidence is missing, the verifier grades down for insufficient
evidence and the next directive must first collect it (never fix missing context by
giving the planner tools).

**Completion stops do NOT cascade team shutdown** (generalizes the architect's
insight): achieving a goal / ending a loop clears the completion run; teams stay
under explicit lifecycle control. **Exception — worker-owned teams:** a loop
worker's iteration-scoped teams ARE reaped when its iteration ends (the worker's
child `CancellationToken` trips → supervised members cancel; graceful-then-force,
5 s). Lead-spawned teams are never touched. Hidden active teams at gate time are a
correctness bug, not a UI issue — the envelope must report active/failed/cancelled
team state.

## 11. TUI (`yaca-tui`)

A background task drains the SSE stream into `AppState`; the render loop polls
crossterm at ~16 ms and `ratatui::draw`s. Three-pane: left = session/message
tree; center = streamed message with tool-call cards (Pending/Running/Completed/
Error); right = team panel (member states + mail snippets + task board) + a
sticky GoalBar (condition / turns / last reason) + a permission-ask modal.
**Rendering is driven purely by the canonical `Event` stream / projected state —
no provider-specific paths** (N1). Snapshot-tested via `ratatui::TestBackend`.

## 12. Cross-cutting

- **Errors**: per-crate `thiserror` enums; `anyhow` only in binaries; `Cancelled`
  threaded end-to-end.
- **Token ledger** (risk #8): a central `TokenLedger` from the first loop records
  actual-or-estimated usage tagged by `session / turn / iteration / provider /
  model / team_id / completion_run_id / role / category` (§6 schema), enforces
  per-turn/goal/loop/team budgets + the loop cost preflight, and feeds the evidence
  envelope + goal/loop status. Needed to *prove* N2, not just claim it.
- **Config**: `$XDG_CONFIG_HOME/yaca/config.toml` (providers, agents, categories,
  goal caps, permissions). Server binds `127.0.0.1:0`; port written to
  `$XDG_RUNTIME_DIR/yaca/port` for client discovery; no external exposure without
  explicit `--bind`.
- **Testing keystone**: `FakeProvider` (scripted canonical events) drives
  deterministic tests of the loop, goal engine, and team orchestrator without
  network. A **provider conformance suite** (recorded SSE fixtures) asserts every
  real provider decodes to the SAME canonical sequence.

## 13. Reconciled decisions (planner conflicts)

1. **Roadmap sequencing — risk's seam-order wins, rust-impl's concreteness wins.**
   rust-impl proposed crate-order (TUI mid, goal/team late); risk proposed
   seam-order (front-load provider normalization, goal/transcript, subagent
   supervision, evidence envelope; rich TUI LAST). **Resolution**: implement.md
   follows the **front-loaded seam order** but every phase carries rust-impl's
   concrete crate deliverables + exact `cargo` validation commands. Rationale: the
   scary unknowns are provider variance + goal↔isolation + supervision, not the
   UI; prove them first, but keep each phase buildable/verifiable.
2. **Goal↔team evidence — Evidence Envelope wins over forced `team_status`.**
   Two of three planners independently converged on a structured envelope; that
   convergence is the strongest signal in the merge. Adopted as §10.
3. **Goal completion does not cascade team shutdown** — architect's solo flag;
   adopted (sound separation of concerns).
4. **Crate count — 9 (middle path).** rust-impl's 8 + a `yaca-client` SDK crate
   (architect's wire-stability/remote-readiness point); team+completion stay as
   `yaca-core` modules (rust-impl) because they're coupled to SessionEngine.
5. **Loop engine — unified driver + engine-owned authority** (D5; from 2 convergent
   loop planners). goal + loop share one `IterationDriver`, diverging only at the
   gate + executor (§9). Authority is encoded in **types**: the engine owns stop
   authority, the verifier is the sole success judge, the planner's output has no
   stop/done field. Both planners independently converged on this + the bounded
   evidence-envelope-fed planner — the strongest signal in this delta. Deep's solo
   additions (no-progress fingerprints, gate-phase resume idempotency,
   evidence_quality, one-driver-per-tree) adopted.

## 14. Integration seams that must stay explicit (the contract surface)

`ProviderRoute → CanonicalEventStream` (provider variance stops here) ·
`CanonicalEventStream → EventStore → Projection` (one source of truth) ·
`Projection → TUI` (never raw provider chunks) · `ToolDefinition →
ToolRuntimeWrapper` (decode→permit→exec→validate→emit→project, in order) ·
`PermissionEngine → ToolRuntime` (no side effect before permission) ·
`SessionSupervisor → MemberTask` (panic/cancel/backpressure containment) ·
`TeamControlPlane → TeamEvidenceEnvelope → TranscriptProjection` (the bridge
between token-efficient isolation and transcript-only judges) · `IterationGate →
NextDirective → IterationExecutor` (the completion control loop; planner cannot
stop) · `WorkerSessionExecutor → child CancellationToken → team reap` (the only new
fragile boundary — iteration-scoped team teardown) · `Verifier/Planner →
TranscriptProjection`/`LoopEvidenceEnvelope` (independence by construction; planner
sees bounded envelopes only) · `TokenLedger` (one accounting system, tagged by
`completion_run_id` + `iteration` + `role`) · `WorktreeManager`/`PaneRuntime` (git/tmux isolated).

## 15. Non-negotiables (gate v0 acceptance)

- Canonical provider event stream + conformance tests before features depend on real providers.
- Transcript projection + Team Evidence Envelope before combining goal + team.
- Supervised member tasks (cancel + panic containment + force recovery) before team breadth.
- Permission snapshots + scoped approval ids before concurrent subagents run tools.
- Event ids + replay + idempotent client reducer before claiming SSE/reconnect support.
- SQLite event durability + deterministic replay before v0 acceptance.
- Hard goal/team budget caps before any autonomous loop is enabled.
- Loop: engine-owned stop authority + planner-has-no-stop-field, the budget ceiling
  + per-iteration caps + no-progress cap enforced before any worker session starts,
  planner-strong / verifier-cheap separation via category routing (not ad-hoc model
  strings), and gate-phase resume idempotency — all before loop mode is enabled.

## 16. Out of scope for v0 (D1)

No LSP, MCP, webfetch, plugin loader, theming beyond a basic palette, web docs,
desktop app, multi-user, billing. No file-backed mailbox (trait only). No remote
SDK polish (HTTP+SSE server exists; SDK is later packaging). The shipped slice is
deliberately **deep** (full team + goal + multi-provider + client/server) so v1
layers extras on a stable substrate without rewriting the loop.

## Plan Review (cross-model gate)

- **Round 1 — oracle, `claude-opus-4-7`** — VERDICT: PASS (D1–D6), BUT same-family
  as the Claude-family planner → cross-family gate NOT satisfied; treated as
  advisory only. (The oracle honestly refused to spoof a GPT model.)
- **Round 2 — codex `gpt-5.5` xhigh (read-only)** — first run VERDICT: **FAIL** —
  D2 (Phase 1 schema absent from design.md; Phase 8 lacked tool I/O + transition
  table) and D4 (rollback only "tag phaseN"; caps lacked numeric defaults). Fixed:
  embedded `0001_init.sql` DDL in §6; added the 12-tool I/O table + state-transition
  table in §8; numeric goal/team caps in §9; concrete rollback steps in
  implement.md. Re-review VERDICT: **PASS** (D1–D6). Cross-family gate satisfied.
- **Round 3 — loop-feature revision — codex `gpt-5.5` xhigh (read-only)** — after
  adding loop mode (the completion engine §9, the two-agent gate, loop tables), three
  re-review passes: R1 FAIL (D4 planner `strategy_change` missing from schema; D5
  cost-preflight/ledger tests unnamed) → fixed; R2 FAIL (D3 `token_ledger` lacked an
  `iteration` column for loop tagging) → fixed; R3 VERDICT: **PASS** (D1–D6).
  Cross-family gate satisfied for the loop revision.
