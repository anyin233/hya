# TUI session reset and subagent visibility

Status: accepted

## `/new` session reset (current behaviour)

The `session.new` command (`slashName: "new"`, aliases include `clear`) currently:

1. navigates to the home route (`route.navigate({ type: "home" })`)
2. clears open dialogs (`dialog.clear()`)

It does **not** abort the old active turn, clear local prompt bookkeeping, or create a new
persisted Session at command time. The previous session is left running on the server until the
user (or another path) resumes or the process stops it. A new persisted Session is created only
when the user later starts work that admits one (for example submitting a prompt from home), not
as part of `/new` itself.

*Historical note:* an earlier statement of this ADR described `/new` as asynchronously aborting
the old turn, clearing prompt bookkeeping, navigating immediately, and lazily creating the next
Session only on the next prompt. That stronger cancellation contract was **not** implemented in
the shipped `session.new` handler; the lighter navigate-and-clear path above is what runs today.
Queued prompts still mean only prompts held before backend readiness; submitted/in-flight prompts
are not a local queue that `/new` drains.

## Subagent visibility (current behaviour)

Subagent visibility is derived from team/member and task-tool presentation rather than synthetic
transcript Messages.

- The live timeline renders a **`TaskMemberRow` per delegated member in every state** (not only
  failed/cancelled terminals): icons `✓` / `✗` / `│`, plus detail lines such as `Working...`,
  current tool title, summary, or duration when completed.
- Members present only in the run tree (not yet attached to a finished task tool row) are
  **synthesized as extra rows on the latest assistant message** so mid-spawn status stays visible.
- The **sidebar has no roster section**. Its feature plugins are Workflow, Context, MCP, LSP, Todo, Modified
  Files, and a footer. The Workflow plugin shows synchronized selection/revision
  availability, run status, Stage and Agent progress, and bounded current work.
  Busy or attention-needed roster entries are **not** listed there; the
  Subagent manager / pane roster remains the full team inspection surface.
- Copy/export continue to use the stored Message transcript only.

*Historical note:* an earlier draft of this ADR said the timeline kept compact activity rows only
for failed/cancelled outcomes and that the sidebar showed busy roster entries. Both were reversed
by the current task-member UI and sidebar composition above.

Considered alternatives: storing synthetic System messages would make exports show subagent
lifecycle, but would duplicate event data and blur Message history with Member lifecycle. Waiting
for `/new` abort confirmation would give a stronger cancellation guarantee, but would make a slow
backend block navigation — and the shipped `/new` does not wait on (or issue) an abort at all.
