# Workflow Sidebar Presentation

## Scenario: Event-driven Workflow state in the TypeScript TUI

### 1. Scope / Trigger

Use this contract when changing the Session Workflow DTO, TypeScript Workflow
validation/presentation, synchronized Session replacement, sidebar feature
plugins, or Workflow PTY coverage.

The TUI is a read-only presentation surface. The owning Session Projection is
the durable state, and the existing SDK/Sync providers are the only transport
and synchronization path.

### 2. Signatures

The server extends normal Session hydration with the shared wire value:

```typescript
type SessionWithWorkflow = Session & {
  workflow?: WorkflowProjection
  workflowActivity?: Array<{ member: string; status: "spawning" | "running"; work: string }>
}

type WorkflowProjection = {
  selection?: { source: string; name: string; revision: string }
  run?: WorkflowRunProjection
  availability?: "available" | "stale" | "unavailable"
}
```

The hya boundary and built-in plugin entry points are:

```typescript
parseWorkflowProjection(value: unknown): WorkflowProjection | undefined
presentWorkflow(value: unknown, membersValue?: unknown): WorkflowPresentation
api.state.session.get(sessionID)
api.slots.register({ order: 50, slots: { sidebar_content } })
```

### 3. Contracts

- Initial Session/bootstrap hydration and existing `session.updated` replacement
  must update the same Session object. Do not add a timer, poll request, second
  SDK client, raw Event fold, or Workflow-specific provider.
- `parseWorkflowProjection` validates unknown wire values before rendering.
  Invalid values become an explicit error presentation with a bounded path,
  not a component crash or silently coerced state.
- No selection renders a muted `Workflow` section with one `none` value.
  Available selection without a run renders `ready`.
- Semantic tones are fixed: ready/running use `info`, completed uses `success`,
  failed/unavailable/invalid use `error`, and stale/cancelled/interrupted use
  `warning`.
- Stage and member arrays retain server declaration/event order. Parallel active
  Stage text is `first +N`; the first Stage must not be selected by sorting or
  by member completion order.
- Agent totals deduplicate only Member ids referenced by Workflow Stages.
  Unrelated run-tree Members must not affect Workflow counts or Session payload
  size. The server emits `workflowActivity` only for linked canonical Members
  whose status is `spawning` or `running`; terminal and unrelated rows are absent.
- Each activity `work` value is capped at 256 Unicode scalar values before wire
  serialization. The view uses the first declaration-ordered active Stage's
  row, caps it again to 24 display characters, then falls back to Stage title/id.
  A failed terminal run shows its first declaration-ordered failed Stage as
  context. Current-work text is capped at 24 characters and renders after
  identity, revision, counts, level, and active Stage fields.
- The sidebar uses existing semantic theme fields and the stock responsive
  visibility rule. It is automatically visible above 120 columns and remains
  hidden at 80 columns unless the user opens the existing sidebar overlay.
- The static plugin host owns teardown. Its slot registration cleanup, slot
  registry disposal, input disposal, and runtime clear must all run when the
  host is disposed.

### 4. Validation & Error Matrix

| Input/state | Presentation |
|---|---|
| Missing or null Workflow extension | `none`, muted |
| Selection + available, no run | `ready`, info |
| Running run | progress/counts/level, info |
| Completed run | terminal Stage count, success |
| Failed run | failed Stage context, error |
| Cancelled or interrupted run | terminal state, warning |
| Stale selected revision | `stale`, warning |
| Missing selected source | `unavailable`, error |
| Wrong object/enum/string/integer shape | `invalid`, error, bounded wire path |

### 5. Good / Base / Bad Cases

- Good: bootstrap shows `ready`; one normal `session.updated` event replaces the
  Session and the same sidebar shows running fan-out and terminal state.
- Good: `alpha`, `beta`, and `gamma` running in declaration order render
  `alpha +2`, even when member ids arrive in another order.
- Base: no selection adds only the muted Workflow section; the existing roster,
  prompt, transcript, and Session navigation remain unchanged.
- Bad: count every child in the run-tree. This includes unrelated Teams and
  produces false Workflow activity.
- Bad: fetch `/workflow` from the component. This creates a second transport and
  loses event-driven replay semantics.
- Bad: render a local DAG or infer Stage status from transcript text.

### 6. Tests Required

- Pure presentation tests: none, ready, running fan-out, completed, failed,
  cancelled, interrupted, stale, unavailable, malformed state, bounded canonical
  Member work, declaration order, deduplicated/scoped member refs, inactive
  terminal members, and failed Stage context.
- Plugin test: Workflow built-in is first, slot order is 50, and static host
  disposal unregisters every slot and clears the runtime.
- Sync test: bootstrap Session Workflow state is observed before one
  `session.updated` replacement; no repeated requests or second client.
- PTY test with real backend and TUI: 140 columns shows selected, running
  fan-out, terminal, and restart-restored state; 80 columns keeps the prompt
  readable and the automatic sidebar hidden.
- PTY transcript warning: `/usr/bin/script` records incremental cursor writes,
  not a reconstructed screen. Assert semantic text introduced by a frame and
  use a fresh-process restored frame for complete terminal counts; do not use a
  brittle whole-screen snapshot.
- Mutation proof: reversing active Stage selection and changing slot order must
  fail the focused presentation and registration tests.

### 7. Wrong vs Correct

#### Wrong

```typescript
createEffect(async () => {
  const response = await fetch(`/session/${sessionID}/workflow`)
  setWorkflow(await response.json())
})

const agents = runTree.children.length
```

This bypasses the configured SDK, creates a second local state owner, and counts
unrelated child Agents.

#### Correct

```typescript
const state = createMemo(() => {
  const session = api.state.session.get(sessionID) as SessionWithWorkflow | undefined
  return presentWorkflow(session?.workflow, session?.workflowActivity)
})
```

This consumes the synchronized Session value, keeps replay and SSE replacement
on one path, and derives display text only from Workflow-scoped references.
