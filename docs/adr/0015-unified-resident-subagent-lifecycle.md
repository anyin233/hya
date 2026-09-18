# Unified resident subagent lifecycle (episodic actors)

ADR-0002 gave subagents a hybrid lifecycle: `transient` blocking-join teams plus
opt-in `resident` actors idling indefinitely. That split produced two spawn
admission paths (the durable-owner route for all-transient batches, the legacy
route for residents), two completion stories (the `TeamEvidenceEnvelope` for
transients, silence for residents), and no garbage collection at all — a
resident that never reports is a zombie, and a resident turn that errors tells
its parent nothing.

We are standardizing the task model instead: **every subagent receives one
definite, terminable task**, may delegate downward exactly once more, and is
represented between tasks by a state document rather than a live session or a
dead log. This ADR supersedes ADR-0002's lifecycle; ADR-0016 supersedes its
communication half (ADR-0011).

## Decision

- **One spawn path, resident only.** Every subagent is a resident actor on
  ADR-0002's actor machinery (mail wake, one turn per wake with coalescing,
  claim fencing, crash recovery). The transient path — `run_team` blocking
  joins, the evidence envelope, the foreground whole-batch admission owner — is
  deleted. `task` is always non-blocking and returns the child handle plus its
  DM channel id.
- **Completion is `report`.** A dedicated tool, gated by the engine: the
  caller's inbox must be drained (no unread mail) and every direct child must
  already be archived. The gate is what makes immediate archive lossless.
- **Terminal handoff.** On an accepted report, one auxiliary model call — the
  subagent's configured model — writes a **state-only** handoff document with
  six sections (Goal / Current state / Files and code / Decisions / Pending
  tasks / Next step): what *is*, never how we got here. The agent carries only
  this document: a revived episode's context is system prompt + latest handoff
  + the reviving DM body — never a replayed transcript. Each handoff
  anchor-updates its predecessor, as compaction summaries already do.
- **Archive is immediate on report.** One transaction: roster removal, actor
  claim release, spawn-budget refund, group-channel membership removal.
  Archived agents are invisible to their parent and unreachable by broadcast;
  the durable event log keeps everything for audit and replay.
- **Revival is a downward DM only** (ADR-0016): re-resolve the runtime binding,
  bump the claim epoch, re-debit the budget, arm a new episode. There is no
  other wake path for an archived agent.
- **Depth is hardcoded to two subagent layers** (a constant, not config).
  Orchestration tools are not advertised at depth 2; admission depth checks
  remain as the API-level backstop.
- **Termination is engine-guaranteed without timers.** The chain is: report
  gate → parent `kill` (force-archive with an engine-synthesized failure report
  and a deterministically degraded handoff) → root-turn teardown force-archive.
  Per-team turn/message budget kills stay as the runaway backstop. An idle
  agent without a report is *never* auto-archived — it may be awaiting an
  answer from its parent.

## Consequences

- The zombie hazard moves from "runs forever" to "never reports", and is
  closed by a responsibility chain rather than timers: a stuck child blocks
  its parent's report gate, forcing the parent to wait or kill.
- A corollary invariant: **while I am live, my parent is live** (a parent
  cannot pass its own report gate while children remain), so upward mail never
  hits an archived peer and revival is exclusively a downward act.
- Revival cost is O(handoff), not O(history). Episodes are bounded contexts;
  the transcript stays in the log, out of the model window.
- Every termination costs exactly one extra model call (the handoff). A failed
  handoff call degrades to a deterministic projection-derived document marked
  `degraded`; terminality never blocks on the summarizer.
- The per-run spawn budget becomes a lease ledger: refund on archive, debit on
  spawn and on revive. Long-lived roots no longer burn out.
- Breaking, accepted: transient teams, the evidence envelope, foreground
  whole-batch reply modes, and `SubagentMode::Transient` minting disappear
  with no compatibility shim.
