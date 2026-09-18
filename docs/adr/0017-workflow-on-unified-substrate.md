# Workflow execution on the unified agent substrate

ADR-0013 and ADR-0014 built user-assembled workflows on a dedicated durable
executor whose stages run as transient teams with per-member workflow routes
(`run_pre_admitted_team_with_workflow`). After ADR-0015 deletes the transient
path, that executor is the last surviving second spawn path — exactly the
duplication the redesign removes.

## Decision

Workflow keeps its **control plane** and moves onto the **unified execution
substrate**:

- Compiled plans, durable run state, and admission through `WorkflowControl`
  (ADR-0013/0014) are unchanged.
- A stage spawn rides the single admission path as an ordinary resident
  subagent: parked activation, stage directive as the arming mail, workflow
  route/guidance attached to the resident slot (the mechanism
  `spawn_resident_parked` + `set_resident_workflow_activation` already
  provides).
- A stage finishes by `report`. `WorkflowControl` consumes `SubagentReported`
  (from the event bus or a store tail) to advance the DAG, mapping report
  outcome and reason to the existing failure classes (retry / abort /
  fallback edges).
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

- One execution substrate for `task` and Workflow. The transient workflow team
  path and its admission variants are deleted.
- The TUI presents workflow agents in the same tree and channels as ordinary
  subagents; there is no parallel workflow presentation surface.
- Workflow retry cost and context behavior follow the handoff model: a retried
  stage resumes from its handoff state, not from its full transcript.
- Stage failure visibility stops being route-finalizer-internal: a failed
  stage produces the same engine-synthesized failure report and degraded
  handoff as any subagent, which is what the DAG consumes.
