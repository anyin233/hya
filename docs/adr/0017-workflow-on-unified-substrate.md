# Workflow execution on the unified agent substrate

ADR-0013 and ADR-0014 originally used a dedicated transient team executor.
Workflow now uses resident episodes for every worker turn and verifier judgment;
the transient executor and its evidence envelope are deleted.

## Decision

Workflow keeps its **control plane** and moves onto the **unified execution
substrate**:

- Compiled plans, durable run state, and admission through `WorkflowControl`
  (ADR-0013/0014) are unchanged.
- A stage spawn rides the single admission path as an ordinary resident
  subagent: parked activation, stage directive as the arming mail, workflow
  route/guidance and completion attached by `ResidentSupervisor::activate`.
- The episode resolves its activation waiter after durable idle settlement,
  terminal report, or forced ending. The controller explicitly accepts both
  settlement (final assistant text) and reports (outcome/text) as Stage
  completion. It adds no per-Stage event listener or Projection polling.
- **Retry is the revival primitive**: a retry edge DM-revives the archived
  stage agent with the retry prompt, inheriting ADR-0015's handoff-carrying
  context and budget re-debit. Workflow retry and subagent follow-up are the
  same operation.
- Verifier-gated stops and engine-owned stop decisions are unchanged: the
  stage's report feeds the verifier; the engine still decides when an
  objective is done.
- The `workflow` tool joins the depth-2-removed orchestration set
  (with `task`, `list_agents`, `search_agent`, `kill`); stage agents appear in
  the user-visible agent tree like any other subagent.

## Consequences

- First Stage activations, loop continuations and verifiers share the `task`
  episode substrate and count against live team capacity. A settled worker
  retains its claim and runtime; a reported worker is revived under the same
  identity through the fenced capacity check. Each judgment gets a fresh
  parked verifier, activated through the same waiter and ended after its result.
- Verifier cancellation waits for its episode's ending, including cancellation
  during initialization. Root driver activity covers the whole run and cleanup.
  Scheduling uses predecessor counts: ready successors and per-Stage verifiers
  proceed without a level barrier (ADR-0013).
- The TUI presents workflow agents in the same tree and channels as ordinary
  subagents; there is no parallel workflow presentation surface.
- Workflow retry cost and context behavior follow the handoff model: a retried
  stage resumes from its handoff state, not from its full transcript.
- Stage failure visibility stops being route-finalizer-internal: a failed
  stage produces the same engine-synthesized failure report and degraded
  handoff as any subagent, which is what the DAG consumes.
