# Design: user-assembled agent workflow composition

Turn hya's loose subagent primitives into a **user-composable DAG workflow
mechanism** (argus-like power, but zero preset combinations). The engine ships
fan-out/fan-in *primitives*; users author stage graphs; nothing hardcodes a
plan→impl→review pipeline.

## Seam inventory (what already exists — reused, not reimplemented)

| Seam | Location | Role in workflow execution |
| --- | --- | --- |
| `MemberSpec`, `run_team`, `pre_admit_team`, `run_pre_admitted_team` | `crates/hya-core/src/subagent.rs` | Per-level member batch spawn + parallel execution + evidence collection |
| `TeamEvidenceEnvelope`, `project_envelope` | same | Bounded per-member status/summary |
| `SubagentLimits`, `SubagentGovernor` | `crates/hya-core/src/orchestrator.rs` | max_depth / per-run budget / concurrency; workflows must NOT bypass it |
| `IterationDriver`, `IterationGate`, `GateOutcome`, `SafetyCaps` | `crates/hya-core/src/completion.rs` | Loop-mode stages reuse this driver instead of a second loop |
| `AgentCatalog::resolve_spawn`, `spawnable` | `crates/hya-core/src/agent_catalog.rs` | Stage agent authorization (`can_spawn` of caller) |
| `TurnBinding::resolve_spawn`, `engine.agent_spec_for_binding`, `agent_roster_for_binding`, `agent_resource_policy_for_binding` | `runtime_registry.rs`, `engine.rs` | Resolution of a stage's agent id into an executable spec under the caller's snapshot |
| Skill discovery roots `skill_dirs_for_workdir` (`<workdir>/.hya/skills`, `$HOME/.config/hya/skills`, first-name-wins) | `crates/hya-tool/src/skill_catalog.rs` | Pattern mirrored for `<workdir>/.hya/workflows` |

## Gap being closed

- No ordering primitive: nothing sequences one team's outputs into a next team.
- No handoff contract: member summaries never feed another member's directive.
- No user-authored composition: batches are model-decided per task tool call,
  not declarable by the user as a reusable workflow file.

## Decision

1. **Artifact**: one workflow per file. Markdown with YAML frontmatter holding
   the full definition (`workflow.hya.md`) or plain `.yaml`. Discovery roots:
   `<workdir>/.hya/workflows` then `$HOME/.config/hya/workflows`
   (first-name-wins). **Zero built-in workflows** shipped.
2. **Module**: `crates/hya-core/src/workflow/{model,parse,plan,run}.rs`.
   In-core because `run.rs` must drive `SessionEngine` team primitives directly;
   app config already depends on core types (`SubagentLimits` precedent).
3. **Graph semantics**: stages carry `needs:` edges; topological levelization
   into parallel batches = fan-out; multiple upstream outputs rendered into a
   downstream directive = fan-in with explicit join contract.
4. **Join contract (user-declarable)**: workflow-level `on_member_failure:
   fail_fast | collect_all`. Ordering: deterministic declaration order; each
   upstream result renders as a bounded section in the consuming prompt template
   via `{{stage_id}}`; user inputs via `{{inputs.key}}`.
5. **Governor pass-through**: every level is ONE batch through
   `pre_admit_team`/`run_pre_admitted_team` (or admission-carrying variants) so
   depth/per-run/concurrency caps hold for user DAGs exactly like task-tool
   batches. Plan-phase validation additionally rejects total members over
   `per_run_budget`.
6. **Loop stages**: `mode: loop` + `verify: {agent, until}` reuses
   `IterationDriver`: worker executor resumes its child session; an independent
   verifier member judges against `until` and emits strict JSON verdicts.
7. Multi-perspective review is plain DAG composition (N reviewer stages fanned
   out from impl, synthesized by a join stage) — no special node type.

## Non-goals (this increment)

- No preset/packaged workflows anywhere in the repo or docs.
- No parallel resume/checkpointing of partially-run workflows (event log keeps
  the evidence; rerun starts fresh).
