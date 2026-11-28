# Authoring Workflows & Evaluators

Hya gives you three ways to build multi-step, verified agent automation:

1. **Compiled Workflows (`*.hya.md`)** — Declarative Stage DAGs compiled by `hya-workflow` and executed on resident subagent sessions.
2. **Goal & Loop Evaluators** — Custom judge models or hooks on the points `hya/goal` and `hya/loop` declare (`hya/goal:evaluate`, `hya/loop:verifier`, `hya/loop:planner`, `hya/loop:should_stop`) for `hya -p` and `hya loop`.
3. **Orchestrator Workflow Programs** — Dynamic JavaScript coordination scripts run by the optional `hya-extra/orchestrator` bundle.

---

## 1. Authoring `*.hya.md` Workflows

Place Workflow files in `<project>/.hya/workflows/*.hya.md`, `~/.config/hya/workflows/*.hya.md`, or package them inside a `Bundle` (`workflow:` key in `bundle.yaml`). Project files override user and bundle Workflows of the same `name`.

### Complete example

```markdown
---
kind: Workflow
name: feature-delivery
description: Plan, implement in parallel, and verify with a loop reviewer.
inputs:
  request: Feature description to implement.
on_failure: collect_all
nodes:
  plan:
    title: Produce implementation plan
    agent: hya-scout
    actor: architect
    directive: Analyze the codebase and write a clear plan for {{input.request}}.

  impl_core:
    title: Implement feature
    agent: hya-task
    directive: Implement the core changes from the upstream plan for {{input.request}}.
    mode: loop
    verify:
      agent: hya-reviewer
      until: Implementation matches the plan and builds cleanly.
      max_iterations: 3

  impl_tests:
    title: Write regression tests
    agent: hya-task
    directive: Add focused tests covering {{input.request}}.

  signoff:
    title: Final architectural sign-off
    agent: hya-scout
    actor: architect
    directive: Check both upstream branches against your original plan.
---
flowchart TD
  plan --> impl_core & impl_tests
  impl_core & impl_tests --> signoff
```

### Graph & input rules

- **Graph syntax:** The Markdown body must start with `flowchart TD`. Declare edges with `a --> b`, fan-out `a --> b & c`, fan-in `a & b --> c`, and `%%` comments. Cycles, self-edges, subgraphs, and Mermaid node labels/shapes are rejected at compile time.
- **Required inputs:** Every key under `inputs:` is required at run time and referenced in directives via `{{input.<name>}}`.
- **Automatic upstream evidence:** When `a --> b`, Stage `b` automatically receives a `<workflow-upstream>` block containing up to 4,000 UTF-8 bytes of output from each direct predecessor in declared edge order.
- **Failure policy (`on_failure`):**
  - `fail_fast` *(default)*: Wait for already-running Stages to settle, skip unstarted Stages, and mark the run failed.
  - `collect_all`: Let independent branches finish while skipping only downstream dependents of failed Stages.

### Loop Stages (`mode: loop`)

Add `mode: loop` and a `verify:` block to any Stage so an independent verifier Agent checks the worker's output after each attempt (up to `max_iterations`). The worker cannot declare itself done—only the fresh verifier session decides when `until` is satisfied.

### Persistent Stage actors (`actor:`)

By default, each Stage runs in a fresh one-shot subagent session. Setting `actor: <key>` (for example `actor: architect` on both `plan` and `signoff` above) routes sequential Stages to **the same resident subagent session** via parent mail, so `signoff` remembers everything `plan` read earlier! (`actor:` cannot be combined with `mode: loop`, and Stages sharing an `actor:` must use the same agent and model route.)

### Per-Stage model routing & fallback

Any Stage (and any `verify:` block) can override its Agent's model and reasoning effort with an ordered failover list:

```yaml
nodes:
  implement:
    title: Implement core
    agent: hya-task
    directive: Implement {{input.request}}.
    model:
      id: openai/gpt-5
      reasoning: high
      fallback:
        - id: anthropic/claude-sonnet-4-6
          reasoning: medium
    mode: loop
    verify:
      agent: hya-reviewer
      until: All acceptance criteria pass.
      max_iterations: 3
      model:
        id: anthropic/claude-sonnet-4-6
        reasoning: low
        fallback:
          - id: openai/gpt-5-mini
```

- **No `#variant` suffixes in Workflow routes:** Always pass a clean `provider/model` in `id:` and put thinking effort in the separate `reasoning:` field (`#variant` suffixes are rejected inside Workflow `model:` blocks, though non-Workflow CLI/API surfaces still accept them). Each candidate resolves `reasoning:` independently.
- **Failover & durable outcomes:** Admission validates the route up front. Pre-stream failures (`UnknownModel`, transport errors, HTTP `429`, HTTP `5xx`) advance to the next `fallback:` candidate, and the root session logs **one route outcome per explicit-route provider stream group** as `WorkflowStageRouteOutcome` (without leaking prompts or keys). TUI and client session sidebars do not render model routes; typed CLI, API, SDK, and `/workflow state` responses expose them.

### Packaging a Workflow inside a `Bundle`

To distribute a Workflow and its custom Agents together as a `.hyabundle`, declare `workflow:` alongside the exact reachable `agents:` closure in `bundle.yaml` (using `prompts/*.md` and `workflows/*.hya.md`). `hya_app::WorkflowControl` exposes list, info, select, run, and state across CLI, TUI (`session.updated` sync), and `/v1`. On restart, nonterminal runs whose owner died become `interrupted`, missing sources show `unavailable`, and edited revisions show `stale` until reselected.

---

## 2. Custom Goal & Loop Evaluators

When running `hya -p "<goal>"` or `hya loop`, the engine—never the worker—owns stop decisions (including iteration ceilings, stall guards, and `--until`/`--while` shell predicates).

You can customize the intelligence behind evaluations in two ways:
1. **Configure a judge model:** Pass `--evaluator-model provider/model` on the CLI, or set `goal.evaluator_model` in `~/.config/hya/bundles/hya%2Fgoal/config.yml` (goal mode) or `loop.evaluator_model` in `~/.config/hya/bundles/hya%2Floop/config.yml` (loop mode).
2. **Write a hook:** Implement a declared hook point in a Bundle/Plugin process (see **[Plugins & Hooks](plugins-and-hooks.md)**):

   | Point | Input | Reply (`null` defers to the next provider) |
   | --- | --- | --- |
   | `hya/goal:evaluate` | `{condition, transcript}` | `{met, reason}` |
   | `hya/loop:verifier` | `{target, transcript}` | `{score, satisfied, evidence_quality, critical_gaps, iteration_summary, reason}` |
   | `hya/loop:planner` | `{target, history, last, planner_notes}` | `{directive, continuity_brief, planner_notes, strategy_change, change_note}` |
   | `hya/loop:should_stop` | `{target, transcript}` | `{stop, reason}`; `stop: true` ends the loop with `hya/loop:should_stop: <reason>` |

---

## 3. Dynamic Workflow Programs (`hya-extra/orchestrator`)

While `*.hya.md` Workflows define static Stage graphs ahead of time, the optional **`hya-extra/orchestrator`** bundle (`bundles/extra/orchestrator`) lets an orchestrator agent write short async JavaScript programs at runtime that call `agents.run({ agent, prompt, result })` across specialized read-only (`orchestrator-scout`), edit-only (`orchestrator-editor`), and shell-only (`orchestrator-runner`) workers, caching read-only scout observations automatically while the workspace file state is unchanged.
