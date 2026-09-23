# Goal/Loop Mode Authoring

> **Audience:** bundle and plugin authors who want to provide the independent
> evaluator for goal mode (`hya-backend -p "<goal>"`) or the verifier/planner
> for loop mode — and operators configuring them.

## Introduction

Goal mode iterates a lead agent on a condition until an **independent
evaluator** reports the condition met. Loop mode runs an executor under a
verifier + planner pair with budgets and no-progress detection. The engine —
not the worker, and not the evaluator provider — owns the stop authority:
iteration caps, wall-clock limits, no-progress detection, and the behavioral
stall guard all live in the engine and cannot be overridden by any provider.

The evaluator/verifier/planner and `loop.should_stop` consult use this
precedence (the first provider returning an applicable result wins):

1. **Explicit `hya/goal-loop` process hooks**: an installed override that
   registers the relevant hook is the selected intelligence provider.
2. **Active-agent selected bundle hooks**: the current binding contributes
   only the hooks reachable through that agent's bundle ownership and
   `hook_refs`.
3. **Other agentless Plugin bundle hooks**: globally installed bundle
   evaluators remain available without an Agent.
4. **Configured global plugin hooks**: ordinary configured plugins are the
   final process-provider fallback. A process that registers the
   `goal.evaluate`, `loop.verifier`, or `loop.planner` hook provides the
   verdicts over the plugin wire protocol.
5. **Bundle prompt + evaluator model**: `hya/goal-loop` supplies the five-part
   contract and six-rule rubric used by the tool-less model evaluator.
6. **Built-in prompt + evaluator model**: retained as a fail-open fallback
   when the bundle prompt is absent or an override is uninstalled.
7. **Workflow verifiers**: workflow stages declare agent-based verifiers via
   `VerifySpec` (see [Workflows](workflows.md)) — independent of the hooks
   above.

## Usage

### Goal mode with a model evaluator

```sh
hya-backend -p "ship the release notes" --evaluator-model anthropic/claude-haud-4-6 --max-iterations 6
```

The evaluator model resolution order is: `--evaluator-model` flag →
`goal.evaluator_model` in `config.yaml` → the worker's current model.

```yaml
goal:
  evaluator_model: anthropic/claude-haiku-4-6
```

### Goal mode with a plugin evaluator

Install any bundle/plugin whose process registers the `goal.evaluate` hook.
Selection is automatic: the explicitly selected `hya/goal-loop` process hook
outranks the active agent's selected hooks, other agentless bundle providers,
and configured global plugins. A transport failure tries the next process
provider; exhaustion or a malformed goal verdict falls back to the bundle
prompt and evaluator model. Removing an installed override restores the
first-party prompt on the next root binding.
See [Plugin protocol](plugin-protocol.md) for the wire contract.

### Loop mode predicates

Loop mode accepts a deterministic exit predicate alongside (or instead of) the
model verifier:

- `--until '<cmd>'` — stop when the command exits 0, continue on exit 1.
- `--while '<cmd>'` — the inverse.

Exit codes 0/1 are the condition's answer; any other exit code or a timeout
means **the condition itself is broken** — the loop stops and reports a
failure instead of silently looking like finished work. When a predicate is
present it outranks the model verdict.

### The goal condition contract

Structured goal conditions follow the five-section shape
(`## Objective`, `## Success criteria`, `## Verification`, `## Boundaries`,
`## Stop conditions`). If you use the structure at all, it must be complete,
and `## Verification` must contain an executable signal (a fenced command, a
backtick-quoted command, or a `$ ` line). Free-form conditions stay allowed;
the structural check runs at entry and rejects incomplete structures.

The first-party `hya/goal-loop` AgentSetBundle contains the `goal-contract`,
`guided-goal`, `evaluator-prompt`, `loop-verifier-prompt`, and
`loop-planner-prompt` skills plus guide and verifier subagents. Its rubric
requires current evidence, claim/verification scope parity, conservative
uncertainty, the complete success set, a distinction between budget and
completion, and compliance with boundaries and stop conditions.

## Interface definitions

### Evaluator hooks (plugin wire)

| Hook | Params | Outcome |
| --- | --- | --- |
| `goal.evaluate` | `{condition, transcript}` | `verdict{met, reason}` or `malformed` |
| `loop.verifier` | `{target, transcript}` | structured verifier verdict |
| `loop.planner` | `{target, history, last_verdict, planner_notes}` | next directive |

### Engine-side invariants (not configurable)

| Invariant | Value |
| --- | --- |
| Hard iteration ceiling | 100 (`cost_preflight`) |
| Default safety caps | 50 iterations / 1800 s wall clock |
| Loop defaults | budget 10, satisfaction threshold 90, no-progress 3 |
| Stall guard (goal mode) | two identical consecutive activity fingerprints stop the run |
| Budget exhaustion | one wrap-up pass, then `BudgetLimited` — never `Achieved` |

### Schema/read-dispatch note

A bundle that declares a `schemas:` entry (e.g. `db://` served by its `lookup`
tool) changes how the built-in `read` resolves those references — the read
stays the entry point and dispatches to the owning tool. See
[AgentBundle authoring](agent-bundle-authoring.md). `read` itself can never be
masked or replaced.
