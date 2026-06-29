# Implementation Roadmap: hya v0

> Execution plan derived from `design.md` (read it first; contracts live there).
> Sequencing reconciles the planner roadmaps (design.md §13.1, §13.5): **front-load
> the dangerous seams** (provider normalization → completion driver + goal/loop
> transcript judging → subagent supervision → evidence envelope), keep the **rich
> TUI and git/tmux late**, and attach **concrete crate deliverables + exact `cargo`
> validation** to every phase.
>
> This roadmap is the source for carving **child tasks** (one per phase or
> phase-group) AFTER this spec task is approved. Each phase is independently
> shippable: it must end green.

## Global conventions

- **Per-phase gate (every phase):** `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` is clean. Libs deny `unwrap_used`/`expect_used` (clippy.toml).
- **Rollback (concrete steps, every phase):** at each green boundary, `git tag phaseN`. **Trigger to roll back:** the phase gate is red, OR a later phase exposes a contract defect in this phase. **Steps:** (1) `git reset --hard phase{N-1}` to restore the last green boundary; (2) for crate-isolated work, instead `git revert` the phase's commits or disable the crate via its cargo `feature` flag (`team`, `goal`, `worktree`, `tmux`, `tui` are all feature-gated) so the rest of the workspace still builds green; (3) re-open the phase's child task with the defect noted. Phases 6–12 are feature-gated precisely so a rollback is local and the prior phase stays shippable. Never roll back past `phase4` (the walking skeleton) without re-approval — everything above depends on it.
- **Testing keystone:** `FakeProvider` (scripted canonical events) drives every loop/goal/team test deterministically — no network in CI.
- **Review gates:** (a) THIS spec — cross-model `plan-review` before any code; (b) a mid-build review after **Phase 7** (the goal↔team hazard convergence point) before widening into full team breadth.

## Critical path & parallelism

Phases 0–4 are the **non-negotiable critical path** (contracts → provider keystone
→ tools → walking skeleton). Phase 6 (completion engine: 6a goal + 6b loop) and
7–9 (team) are the core D4/D5 deliverables; after Phase 5 they can be split across
developers (6 and 7–9 share the Evidence Envelope, so coordinate that seam). Phases
11–12 (git/tmux, rich TUI) are intentionally last — fragile and visible, but they
do not answer the core architecture questions.

---

## Phase 0 — Workspace bootstrap  ·  crate: all
- 9-crate cargo workspace; pinned `[workspace.dependencies]`; `rustfmt.toml`; `clippy.toml` (deny unwrap/expect in libs); `xtask` skeleton (`cargo xtask migrate`); CI (build + clippy -D warnings + test).
- **Validate:** `cargo build --workspace`. **Rollback:** tag `phase0`.

## Phase 1 — Contracts + persistence  ·  `hya-proto`, `hya-store`
- All newtype ids; `Message`/`Part`/`ToolPartState`/`Event`/`Envelope`/`ModelRef`/`ToolSchema` (tagged enums, design.md §3). Apply migration **`0001_init.sql` and `0002_pragmas.sql` verbatim from design.md §6** (10 tables incl. `event_log`, `team_*`, `goal`, `token_ledger`; WAL + busy_timeout=5000 + foreign_keys=ON). `SessionStore::{append_event, replay, project}`. Idempotent reduce/projection fn shared with `hya-client`.
- **Validate:** `cargo test -p hya-proto -p hya-store`; migration runs clean against a tempdir SQLite; property test `replay(log) == projection`; reducer idempotency test (applying an event twice is a no-op). **Rollback:** per Global conventions; tag `phase1`.

## Phase 2 — Provider normalization KEYSTONE  ·  `hya-provider`   [front-loaded; risk #1]
- `Provider`/`Protocol`/`Route` traits; `ProviderRouter`; `FakeProvider`; OpenAI Chat route+protocol (encode + SSE decode → canonical `Event`s); route capability matrix + preflight.
- Conformance harness with recorded SSE fixtures: streamed text, one tool call, multiple/parallel tool calls, malformed partial tool-JSON, model refusal, provider abort, usage reporting.
- **Validate:** `cargo test -p hya-provider` — fake roundtrip + OpenAI fixtures + a **provider-parametric** test asserting identical canonical event sequences (modulo ids/tokens/timestamps). **Gate:** must be green before any layer depends on a real provider. **Rollback:** tag `phase2`.

## Phase 3 — Tools + permission plane  ·  `hya-tool`
- `Tool` trait, `ToolCtx`, `ToolRegistry`; built-ins `read/write/edit/glob/grep/shell`; `PermissionPlane` (merged last-rule-wins rules, `ask` pending oneshot channel, immutable per-turn snapshot, narrow-only child derivation).
- **Validate:** `cargo test -p hya-tool` — each tool happy/denied/cancelled; property tests for last-rule-wins, scope match, snapshot immutability, child derivation; concurrent overlapping-scope asks don't cross-satisfy. **Rollback:** tag `phase3`.

## Phase 4 — Walking skeleton: single-agent loop + minimal server/client  ·  `hya-core`, `hya-server`, `hya-client`   [crosses every seam]
- `SessionEngine::{create, admit_user_prompt, subscribe, cancel_turn, resume}`; `AgentLoop::run_turn` (parallel tool dispatch, cancellation threaded); `EventBus` (broadcast). Minimal axum `/sessions`/`/messages`/`/events`(SSE, replay + `resync` on lag); minimal `hya-client`; a headless driver binary for QA.
- **Validate:** `cargo test -p hya-core --test turn_loop` (FakeProvider text→tool→result→text round-trip); server integration via `tower oneshot` (create+admit, scrape SSE); **reconnect-from-seq projection == uninterrupted projection**. Manual: `hya serve` + `curl /events/{ses}`. **Rollback:** tag `phase4`.

## Phase 5 — Multi-provider hardening  ·  `hya-provider`   [risk #1, #12]
- `AnthropicMessagesRoute` + protocol (incl. `tool_use` streaming); `OpenAICompatibleRoute` (Ollama/vLLM, user base_url); capability preflight that rejects tool-use turns on incapable local routes BEFORE the turn.
- **Validate:** conformance suite across all 3 providers; the parametric loop test now runs provider-agnostic (no provider branches outside adapters); preflight-rejection test. **Rollback:** tag `phase5`.

## Phase 6 — Completion engine: autonomy driver + goal + loop  ·  `hya-core::completion`   [front-loaded before team; risk #3, #11; D5]
Carve into **two child tasks**. Build the SHARED driver once; goal and loop are then two small swaps (design.md §9).
- **6a — driver + goal gate:** `IterationDriver<G,X>` + `IterationGate`/`IterationExecutor` traits + `SafetyCaps` (driver-enforced); `LeadTurnExecutor`; `GoalGate` wrapping `GoalEvaluator` (deterministic + cheap-model impls, transcript-only, no tools); goal caps (turns=50/time=1800s/tokens=2M); directive composition with last-reason feedback; `goal_set/status/clear`; non-interactive `hya -p "/goal …"`.
  - **Validate:** `cargo test -p hya-core --test goal_loop` — scripted `met=false×3 → met=true` ⇒ exactly 4 iterations + correct events; false-claim ⇒ not met; cap ⇒ stop; resume resets baselines; malformed eval JSON ⇒ `met=false` + counts toward cap.
- **6b — loop mode + two-agent gate:** `WorkerSessionExecutor` (fresh worker child-session per iteration, per-iter caps 30 turns/500k tokens, planner-carried continuity); `LoopGate` = `LoopVerifier` (cheap, evidence_quality schema) + `LoopPlanner` (strong; output `{directive, continuity_brief, planner_notes, strategy_change, change_note}` — NO stop/done/satisfied field); engine-owned stop authority; early-stop short-circuit (threshold 90 + critical_gaps empty + evidence≥supported); no-progress detection (directive/gap fingerprints, `max_no_progress=3`); **cost preflight** (worst-case spend before non-interactive start); **TokenLedger** loop tagging (`completion_run_id`+`iteration`+`role`); `loop_set/status/clear/pause/resume`; `loop_run`/`loop_iteration` tables (migration 0003) + `gate_phase` resume idempotency `(loop_id, iter, role)`; non-interactive `hya -p "/loop …"`.
  - **Validate:** `cargo test -p hya-core --test loop_gate` — scripted verifier scores `40,76,93` ⇒ exactly 3 workers / 3 verifiers / 2 planners / terminal `Satisfied`; `planner_done_ignored_when_verifier_false`; `planner_skipped_on_terminal_success`; `budget_exhaustion_wins`; `exact_budget_mode` (`stop_when_satisfied=false`); `no_progress_detection`; `repeated_directive_rejected` (fingerprint repeat without `strategy_change=true`+`change_note` ⇒ typed error/forced replan, no worker rerun); `cost_preflight` (worst-case spend = N × per-iter caps + planner/verifier computed and surfaced before a non-interactive loop starts; rejects `budget > 100`); `token_ledger_complete` (each iteration records worker+verifier+planner+team usage tagged by `completion_run_id`+`iteration`+`role`, incl. estimated local usage); malformed verifier/planner JSON handled; `cancel_fans_out`.
- **Rollback:** per Global conventions (features `goal`, `loop`); tag `phase6a`/`phase6b`.

## Phase 7 — Subagent substrate + Team Evidence Envelope  ·  `hya-core`   [the big D4 hazard; risk #2, #4]  ← REVIEW GATE
- Supervised member tasks: child sessions under the lead, `tokio::spawn` + child `CancellationToken` + bounded `mpsc` inbox + `watch` state + `JoinHandle` watchdog (panic isolation → `Failed`, server survives). Minimal team run (lead + 2 fake members). **Team Evidence Envelope** projected into the session transcript; the **completion engine (goal AND loop)** consumes it via `TranscriptProjection`; the loop planner additionally consumes the **Loop Evidence Envelope** (bounded; no child transcripts). Iteration-scoped team **reap** on worker-session end (graceful-then-force).
- **Validate:** fake lead delegates to 2 children (1 ok, 1 fail) ⇒ transcript receives the structured envelope and **NOT** member assistant text (assert by message-id provenance); BOTH the goal verifier and the loop verifier/planner judge the envelope correctly (planner prompt contains envelope fields but no child assistant text — provenance test); a panicking child does not take down the server; force-cancel terminates all children; a worker iteration that spawned a team leaves **no active team** at gate time.
- **REVIEW GATE:** stop and review before widening into full team breadth.  **Rollback:** tag `phase7`.

## Phase 8 — Full team control plane + 12 tools + lifecycle  ·  `hya-core::team`   [risk #10]
Carve into **three child tasks** (each independently shippable + testable); each `team_*` tool must be built against the I/O contract table and the state-transition table in **design.md §8**, not improvised.
- **8a — backends:** `MailboxBackend`/`TaskBoardBackend` traits + `InMemoryMailbox`/`InMemoryTaskBoard` (RwLock + per-team broadcast); persist `mail`/`task_board`/`team_member` rows (incl. both `background_task_id` + `session_id`). **Validate:** unit tests for send/poll/ack + task CRUD + ordering.
- **8b — state machine:** implement the typed transition table from design.md §8 as a `TeamState`/`MemberState` FSM that rejects invalid `(from,event)` pairs at the command boundary. **Validate:** a table-driven test asserting every allowed transition succeeds and a representative set of disallowed ones return the typed error; `team_delete{force:false}` with active members → rejected; `force:true` → ForceDeleting→Deleted.
- **8c — the 12 tools:** thin `Tool` impls matching the design.md §8 I/O contract; lead-only broadcast; `MailKind` rejects shutdown kinds; closure-ready flow; permission-gating per the `who` column.
- **Validate (phase):** integration — create→assign tasks→member messages→shutdown_request→approve/reject→force-recover one member→delete with **no orphaned active members**; assert lead history never contains member assistant text (message-id provenance). **Rollback:** per Global conventions (feature `team`); tag `phase8`.

## Phase 9 — Categories + skill injection + multi-provider members  ·  `hya-core::team`
- `category → model + fallback-chain + prompt-append` resolver; skill-content injection into member system prompt; mixed-provider members; per-category budgets; question-denied member permissions; preflight rejects incompatible category routes.
- **Validate:** `cargo test --test category_routing` — 4 members / 4 categories ⇒ 4 distinct provider/model calls via `FakeProvider`; context isolation preserved; aggregate token usage (via `TokenLedger`) reported in `team_status`. **Rollback:** tag `phase9`.

## Phase 10 — SQLite + resume hardening under team load  ·  `hya-store`   [risk #6]
- WAL + single-writer discipline; appends are short txns; `busy_timeout` + bounded retry; queue-depth/write-latency metrics; persist session tree + team + goal + **loop_run/loop_iteration (incl. `gate_phase`)** + ledger; mid-run kill/restart resume incl. **mid-loop gate-phase idempotency** (re-run a phase ≤ once; reuse a persisted directive).
- **Validate:** concurrency stress (many fake child sessions emitting deltas + tool events at high rate) ⇒ **no `database is locked`**; per-session event order preserved; kill/restart ⇒ resume with correct session tree, goal/loop state, team status, parts, ledger; `replay == projection`; `resume_each_gate_phase` (crash/replay from `EvidenceBuilt`/`Verified`/`Planned` ⇒ idempotent provider calls, no double-count in the TokenLedger). **Rollback:** tag `phase10`.

## Phase 11 — Worktrees + tmux  ·  `hya-core::team`   [risk #7; fragile, late]
- `WorktreeManager` (shell-out `git worktree add/remove`, owned-resource registry, dirty-state report, allowlisted cleanup); `TmuxPaneManager` (`tmux split-window` + `send-keys "hya tail-session <id>"`); cleanup on `team_delete` + force; capability-degrade with a clear error when `tmux` absent.
- **Validate:** `cargo test --test worktree_lifecycle -- --ignored` (needs `git`) in a temp repo: create team w/ worktree-per-agent, harmless edits, per-member dirty state, shutdown, reject-delete-while-active, force-recover, **clean only owned resources** (never the main checkout); tmux tests gated on `tmux` present. **Rollback:** tag `phase11`.

## Phase 12 — Rich ratatui TUI  ·  `hya-render-tui`   [last: fragile + visible]
- Full three-pane (session tree / message stream with tool cards / team panel) + sticky **GoalBar AND LoopBar** (LoopBar: target, iter N/budget, last score, gaps, next directive, spend + projected burn, stop policy) + permission-ask modal; reconnect handling (render projected state only); token/cost display. SSE-drains into `AppState`; ~16 ms render loop.
- **Validate:** `ratatui::TestBackend` snapshot tests (incl. LoopBar states); **manual QA (USE IT)**: single-agent tool turn, a goal that loops once, a 3-member team, approve+deny a permission, disconnect/reconnect, resume — final transcript + token ledger match server state. **Rollback:** tag `phase12`.

## Phase 13 — E2E manual QA + polish  ·  `hya-cli`
- Umbrella binary: `hya` (interactive TUI + embedded server), `hya serve`, `hya -p "…"` (headless goal/loop), `hya tail-session <id>`. README + sample agent files + sample category/permission/loop config.
- **Validate:** `cargo test --workspace --all-features` clean; **manual end-to-end (FULL MANUAL QA contract): (1) a goal that requires a 3-member team, watched to achieve through the real TUI; (2) a loop that stops early when satisfied; (3) a loop with `stop_when_satisfied=false` that exhausts N** — drive a real terminal, run happy path + bad input + `--help`. **Rollback:** tag `phase13`.

---

## Estimate & cut lines

Rough single-developer serial estimate ≈ 22–25 engineering days (planner
estimates ranged 22–25 days / 9–11 weeks depending on parallelism). If schedule
slips, cut in this order (preserves confirmed v0 intent): theming/visual polish →
remote SDK polish → file-backed mailbox (keep trait) → dynamic skill loading
(keep static skill-content) → lower the loop **planner** model tier (cost is
multiplicative by N) → restrict (don't drop) local-provider capabilities → tmux
panes (keep worktree isolation). **Require explicit user re-approval** to cut any
of: multi-provider, the transcript-only verifier (goal OR loop), the Evidence
Envelope, the loop's engine-owned stop authority + verifier-as-sole-judge + loop
caps, scoped member permissions + supervised cancellation, the durable event/replay
protocol, or worktree-per-agent.
