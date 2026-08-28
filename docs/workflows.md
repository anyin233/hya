# Workflows

A **workflow** is a user-authored file that composes hya's subagent teams into
a reusable DAG of stages. hya ships **zero** built-in workflows — you assemble
your own, stage by stage; nothing hardcodes a plan→impl→review pipeline.

## Where workflows live

| Root | Path |
| --- | --- |
| Project | `<workdir>/.hya/workflows/*.yaml` (or `.yml`, `.md`) |
| User | `$HOME/.config/hya/workflows/` |

Project files shadow user files with the same workflow name
(first-name-wins, mirroring skill discovery). A markdown file must carry the
whole definition in YAML frontmatter (`workflow.hya.md` style); everything
after the closing fence is free-form documentation.

## Definition schema

```yaml
name: feature
description: explore then implement from two angles, then review both
inputs:
  target: what to explore       # values supplied per run; all keys required
on_member_failure: fail_fast    # fail_fast (default) | collect_all
stages:
  - id: explore                 # kebab/snake id, unique
    agent: explorer             # any agent your session may spawn (can_spawn)
    prompt: "Explore {{inputs.target}}"

  - id: impl_a                  # fan-out: same needs -> one parallel batch
    agent: builder
    needs: [explore]
    prompt: "Implement A per:\n{{explore}}"

  - id: impl_b
    agent: builder
    needs: [explore]
    prompt: "Implement B per:\n{{explore}}"

  - id: review                  # fan-in: joins BOTH upstream sections
    agent: reviewer
    needs: [impl_a, impl_b]
    prompt: |
      Review both implementations:
      {{impl_a}}
      {{impl_b}}
```

### Rules

- **DAG only**: `needs:` edges are validated at plan time; cycles,
  self-dependencies, dangling refs, and forward template references are
  rejected before anything spawns.
- **Placeholders**: `{{inputs.key}}` for run inputs, `{{stage_id}}` for an
  upstream result. Each upstream renders as a bounded section (default cap
  4000 chars) headed by its stage id and terminal status, in declaration order.
- **Fan-out/fan-in**: stages whose dependencies resolve at the same topological
  level execute as ONE parallel member batch. Joins see every upstream's
  section.
- **Join contract**: `on_member_failure` is yours to declare. `fail_fast`
  aborts downstream work on any failed member. `collect_all` continues and
  marks failed upstreams as `FAILED` in the joined directive so the joining
  stage can reason about partial results.
- **Loop stages**: `mode: loop` with a required `verify: {agent, until}` block
  iterates through hya's shared iteration driver. The verifier runs in a fresh
  child session per judgment (independent stop authority) and answers strict
  JSON `{"met": bool, "reason": str}`; malformed output counts as not-met.
- **Budgets are non-negotiable**: execution goes through the same governed team
  path as the task tool (`pre_admit_team` / `run_pre_admitted_team`), so
  max-depth, streaming-concurrency, and per-run spawn budgets from
  `[subagents]` config bound your DAG exactly like model-decided batches.
- **Failure semantics**: a stage whose member errors during streaming is
  reported `failed`. Under `fail_fast` (default) every downstream stage is
  skipped and the run ends failed; under `collect_all` remaining stages still
  run and failed upstreams are declared `FAILED` inside joined directives. A
  loop stage capped without verification keeps its worker output but does not
  carry the verified marker. Unknown or unauthorized stage agents and workflows
  whose declared stage count exceeds the run budget are rejected before any
  member spawns.

## CLI

```sh
hya-backend workflow list            # discovered workflows + stage ids
hya-backend workflow info feature    # full graph, join contract, verify blocks
```

## Running workflows

### From the shell

```sh
hya-backend workflow list                  # discovered workflows + stage ids
hya-backend workflow info feature          # graph, join contract, verify blocks
hya-backend workflow run feature \
  --input target=src/parser.rs              # execute the DAG now
```

`workflow run` executes in-process against your configured providers, printing
one row per stage (`[done]` / `[failed]`) with its bounded output; `--json`
emits the machine-readable report instead. Every `--input key=value` pair
splits on the first `=` (values may contain further `=` signs) and must match
the declared inputs exactly: a missing declared input or an undeclared key
aborts with a clear error before any stage spawns, as does a run that ends
`failed`/`cancelled`.

### From an agent session

Agents get the same power through the governed **`workflow` tool**:

- `{"action": "list"}` summarizes discovered files.
- `{"action": "run", "name": "feature", "inputs": { ... }}` launches the DAG
  mid-session. Permission-wise a run asserts the task class scoped to
  `workflow:<name>`, so existing ask/deny rules for subagent work apply.

Both surfaces route through the identical core executor, which uses the same
pre-admitted team batch path as the task tool — see *Budgets* below.

#### Member execution contexts mirror the task tool

Every stage runs with the context its TARGET agent would get from the task
tool, resolved up front through the same engine accessors:

- **Authorization**: each stage agent must be spawnable by the calling agent
  (`can_spawn`), checked before any member session is created — a typo aborts
  the run instead of failing mid-DAG.
- **Delegation**: a stage member's own reachable roster comes from the STAGE
  agent's `can_spawn`, never from the caller's broader roster. A stage whose
  agent cannot itself spawn further targets gets a refused `task` call inside
  that stage; delegation still works when the target lists it.
- **Resources & sidecars**: resource/tool policies and Bundle sidecar factories
  resolve per stage agent exactly as for task-spawned members, so bundle tools
  and hooks stay available (and absent when the bundle declares none).

The same resolution applies to loop workers across iterations and to every
independent verifier judgment session.

Authoring a file makes it discoverable immediately; there is no install step.

## Library surface

`hya-core::workflow` exposes `WorkflowDef` parsing (`load_workflow_file`,
`load_workflow_by_name`, `discover_workflow_files`), planning (`build_plan`:
cycle detection + levelization + placeholder closure), and execution
(`run_workflow`) over any `SessionEngine` + `TurnBinding`. See
ADR-0013 for the design rationale.
