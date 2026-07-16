# CLAUDE.md — yaca

> Project overview, component map, change guidance, and verification live in
> `AGENTS.md`. This file adds the **plan-execution agent routing** rules.

## Plan-Execution Agent Routing

When executing an **approved** implementation plan, the orchestrator (run Fable
in the main loop for this) does not do the edits itself — it dispatches each plan
step to one of two specialized executor subagents via the Task tool, then merges
their structured reports and moves to the next step.

### The two executors

| Agent (`subagent_type`) | Model | Owns |
| --- | --- | --- |
| `plan-executor-heavy` | opus | Dangerous / deep-reasoning steps |
| `plan-executor-bulk` | sonnet | Large-volume but low-risk steps |

### How to route each step

Route to **`plan-executor-heavy`** when the step involves:
- A breaking change to a public API, trait, `Event`/`Envelope`, or wire/serialization format
- A feature rewrite or a cross-cutting refactor with wide blast radius
- Concurrency, session/turn lifecycle, event-bus, or projection/reducer changes
- A migration, or any edit where a subtle mistake silently corrupts behavior
- Anything needing blast-radius mapping and invariant reasoning before editing

Route to **`plan-executor-bulk`** when the step is:
- Mechanical and rule-driven (mass rename, apply a settled pattern to N sites)
- Repetitive scaffolding / boilerplate following an existing example
- Adding tests, docstrings, or `derive`s across many files
- Wiring up many similar, well-specified call sites
- High in volume but low in per-edit risk

### Routing decision rule

Ask: *"Could a wrong edit here silently corrupt behavior or break callers?"*
- **Yes** → `plan-executor-heavy`.
- **No, it's just volume/repetition** → `plan-executor-bulk`.
- **Unsure** → default to `plan-executor-heavy`; under-routing a risky step is
  the costly mistake, over-routing merely costs tokens.

### Escalation

`plan-executor-bulk` is instructed to STOP and return
`Status: escalate-to-heavy` if a "simple" step turns out to be a breaking change
or needs invariant reasoning. When the orchestrator receives that status, it
re-dispatches the step to `plan-executor-heavy`.

### Dispatch contract

- Give each executor **one step (or a tightly-scoped group)**, not the whole plan
  — they execute, they do not re-plan.
- Executors return structured reports (`Status`, files modified, validation
  results, risks/coverage). The orchestrator reads these, not the user; relay
  only what matters to the user.
- Independent steps with no ordering dependency may be dispatched to executors
  concurrently (multiple Task calls in one message).
- Every step still obeys `AGENTS.md`: TDD gate, verification commands for the
  touched area, and the commit/push rules.

## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues for `anyin233/hya`; external PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Triage uses the canonical label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Domain docs use a single-context layout: root `CONTEXT.md` plus root `docs/adr/`. See `docs/agents/domain.md`.
