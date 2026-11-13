# Why retain Workflow levels after removing their execution barrier?

API info and durable plan/state responses expose each Stage's zero-based
`level`, and rendered Stage guidance includes it. Keep this topological metadata
for display. Execution counts unfinished direct predecessors instead. Each Stage
owns its worker and verifier future, so `a --> c` can complete `c` while an
independent `b` runs, and a verifier does not wait for unrelated workers.

# How are evidence and shared actors ordered?

Reports occupy compiled Stage indices. Rendering already visits each Stage's
compiled predecessor indices, so completion order cannot reorder join evidence.
Ready Stage indices are ordered; a busy actor key remains reserved until its
whole Stage finishes. Other ready keys proceed. Same-level actor reuse is now
valid: the old compiler rejection depended on an execution barrier that no
longer exists. Only simultaneously ready uses are ordered; unready uses cannot
block a predecessor through an implicit actor dependency.

# What happens to failure and cancellation with ready scheduling?

`fail_fast` stops new activations after the scheduler observes a failed Stage,
then drains those already activated. `collect_all` runs independent eligible
branches. Both skip successors of failed/skipped Stages, as explicitly required
by M4-14 (this replaces earlier failed-evidence join behavior). Cancellation stops
new activation. An execution error cancels and drains active Stage futures before
cleanup; dropping a verifier future during registration or an ending await would
lose ownership. Every committed registration receipt is retained immediately,
and unsuccessful runs await all run-owned actors and descendants under the
existing root-driver activity guard. Mail targets, claim semantics and capacity
preflight are unchanged.

# How does the partial-registration regression stop between ready siblings?

Hold the first committed worker link in a synchronous event hook, before the
scheduler registers the second ready sibling. Use `block_in_place` around the
bounded test barrier so Tokio can run store/pool continuations. A concurrent
parked actor then consumes the final claim. The second registration must return
typed overload, release the first sibling's claim without activating it, and
retain the unrelated claim. An `agent_spawn` hook only blocks initialization;
it cannot enforce this registration boundary for parked actors.

# When does a never-started Stage become skipped?

The scheduler writes `WorkflowStageFinished(Skipped)` when it skips a blocked
Stage. Independent branches can remain Running while that skip is visible.
Remaining unstarted Stages are Skipped on cancellation too, matching the
projection; only activated cancelled Stages are Cancelled. Evidence rendering
accepts successful direct predecessor outputs only, so it no longer carries
an enum for unreachable failed/cancelled/skipped evidence.
