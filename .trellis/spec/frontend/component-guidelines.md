# Component Guidelines

> How components are built in this project.

---

## Overview

The current TUI uses SolidJS with OpenTUI under `packages/hya-tui-ts`. Extend
the existing components, contexts, dialogs, routes, command registry, and
feature-plugin slots before creating a new framework boundary.

`src/upstream/` is the retained frontend implementation. `src/hya/` contains
hya-owned product, platform, audit, static-host, and SDK-spine integration.

---

## Component Structure

- Keep rendering declarative and derive view state from existing Solid contexts.
- Keep HTTP/SSE access in the SDK and sync contexts; components should not create
  parallel clients or poll backend state.
- Keep route transitions in the route context and command actions in the command
  registry so keybindings, palette actions, and UI behavior stay aligned.
- Put repeated app-wide UI in the existing feature-plugin slots instead of
  wrapping the application in another layout system.
- Keep hya-specific boundary adaptation in `src/hya/` when it does not belong in
  the retained upstream implementation.

---

## Props and State

Use explicit props for local display inputs and existing providers for shared
runtime state. Follow Solid's accessor/store semantics; do not copy React hook
patterns or destructure reactive props in a way that loses tracking.

Validate unknown values at SDK, persistence, or route boundaries. Internal
components should consume the normalized types produced there.

---

## Styling and Accessibility

Use the existing OpenTUI primitives and semantic theme values. Important state
must have a text or symbol signal in addition to color. Preserve usable prompt,
dialog, and transcript layouts on narrow terminals.

---

## Common Mistakes

- Do not reintroduce a Rust TUI or place shipped interactive behavior outside
  `packages/hya-tui-ts`.
- Do not bypass `@opencode-ai/sdk/v2` with a second HTTP client.
- Do not duplicate synchronized server state in component-local stores.
- Do not import excluded OpenCode server, worker, updater, web, or desktop code.
- Do not edit generated logo or epilogue data by hand; use the existing asset
  generation script.
---

## Scenario: Hya-Owned Coding-Tool Presentation From SDK Parts

### 1. Scope / Trigger

- Trigger: changing completed Read, Edit, Grep, Write, Bash, or hidden `shell`
  rendering; the projected tool-part shape; semantic metadata; responsive
  layout; or Session reopening/replay.
- Applies to `packages/hya-tui-ts/src/hya/coding-tool-presentation.tsx` and
  its one dispatch point in the retained Session route. `src/hya` owns the
  adapter and normalizer; `src/upstream` retains the surrounding Session,
  SyncProvider, theme, width, path, code, and diff primitives.
- This component is a read-only view. Backend schema validation, permissions,
  event persistence, projection, and result caps remain backend/SDK owners.

### 2. Signatures

- `presentCodingTool(part: ToolPart): CodingToolView | undefined` validates a
  projected SDK `ToolPart` and returns an allowlisted semantic view only for a
  completed coding tool. `undefined` selects the existing inline/error or
  generic fallback.
- `CodingToolPresentation(props)` renders either the `CodingToolView` already
  normalized at the retained Session route or, for isolated callers, one
  projected `ToolPart`. It receives `width`, `diffStyle`, and `diffWrapMode`,
  owns only local collapse state, and never owns synchronized server state.
- The relevant projected part fields are `type: "tool"`, `tool`, `callID`, and
  `state`; completed state supplies `input`, `output`, `title`, and `metadata`.
  Grep display metadata is `metadata.display.groups[]` with
  `{path, rows: [{line, text, isMatch}]}`. Read/Edit/Write display metadata
  uses allowlisted `type`, `path`, `text`, `lineStart`, `lineEnd`,
  `totalLines`, and `truncated` fields. The view combines nested display
  truncation with top-level `truncated` and every backend result-cap fact
  listed below.
- SyncProvider owns initial hydration and `message.part.updated` replacement.
  The route calls `presentCodingTool` once and passes that normalized `view` to
  `CodingToolPresentation`; no route/provider hydrates a second message store
  or schedules a presentation timer.

### 3. Contracts

- Read and Write completed views show a file-derived title, grammar-aware
  syntax spans, stable line numbers, the backend `lineStart` offset, and a
  bounded reversible collapse affordance. Directory and attachment results,
  raw text, truncation notices, and diagnostic text retain readable fallback
  behavior when their metadata is not a valid file view.
- Edit uses the existing semantic `<diff>` primitive. It is unified at 80
  columns and keeps removed and added rows distinct; it may split only above
  120 columns (unless the selected wrap mode explicitly requests stacking).
  The diff and status text are derived from bounded backend metadata, not from
  an additional file read. Completed Edit/Write diagnostics keep only the first
  three severity-one entries with `range.start` and label them
  `Error [line:column]` with one-based display coordinates.
- Grep renders each `metadata.display.groups[]` entry as its own titled file
  block, preserves backend result order, labels match rows distinctly from
  context rows, derives grammar per file, and wraps/clips inside the available
  transcript width. A missing or malformed group falls back without rendering
  arbitrary metadata.
- Bash and hidden `shell` normalize to one command/output view. `metadata.exit`
  may be null for timeout or signal termination; output and textual status stay
  visible in that case. Syntax styling applies only to the command; output is
  plain text with ANSI escapes removed. The title may identify the command, but
  `env` values and unknown input keys never render. Nonzero exit, timeout,
  cancellation, and artifact/truncation notices retain textual status signals
  in addition to semantic colors.
- `presentCodingTool` is the only validation boundary for this view. It accepts
  the projected SDK state, allowlists fields, bounds rows/text, and returns
  `undefined` for malformed or unsupported completed data. It never casts raw
  JSON in multiple components, fetches a tool result, polls, replays Events, or
  stores a second copy of backend state.
- The normalizer's hard display bounds are title 8 KiB, path 16 KiB, pattern
  16 KiB, command 64 KiB, general display text 512 KiB, diff 1 MiB, output
  512 KiB, at most 200 Grep groups and 4,096 rows, and at most 200 input
  diagnostics with each message capped at 16 KiB. It strictly validates every
  diagnostic entry before retaining only the first three positioned
  severity-one diagnostics. Top-level `truncated` and every backend result-cap
  flag (`outputTruncated`, `titleTruncated`, `displayTruncated`,
  `diffTruncated`, `attachmentsTruncated`, `diagnosticsTruncated`,
  `warningsTruncated`, `rowsTruncated`, `groupsTruncated`,
  `metadataTruncated`, `unknownFieldsDropped`, and `envelopeTruncated`) remain
  one visible truncation fact. Exceeding a bound selects the safe fallback; it
  never silently truncates a secret into a rendered field.
- Pending, streaming, permission, denied, malformed, diagnostic, error,
  directory, attachment, and unsupported states continue through the existing
  inline/generic/error components. Syntax grammar/parser failure falls back to
  readable plain text rather than hiding the result or throwing during render.
- Backend truncation is authoritative and visibly marked; local collapse is a
  reversible display choice. Do not treat collapsed text as a new server result
  or silently expand a backend-truncated payload.
- Initial Session hydration and one `message.part.updated` replacement converge
  through `SyncProvider`. Reopening the same Session consumes the projected
  completed part and renders the same title, status, metadata, and bounded
  content without an extra presentation request.
- Use semantic theme roles (`text`, `textMuted`, `accent`, `success`, `warning`,
  `error`, and `info`) and textual labels. Do not add raw color literals,
  decorative nested cards, a Rust renderer, or a second HTTP/SSE client.

### 4. Validation & Error Matrix

| Projected state/input | Component behavior |
| --- | --- |
| Completed Read/Write with valid file metadata | Titled syntax-aware numbered block with correct offset and bounded expand/collapse |
| Completed Edit with valid diff metadata | Existing semantic diff; unified at 80 columns and split only in wide mode above 120 columns |
| Completed Grep with valid groups | One titled match/context block per file, backend order preserved, file grammar selected independently |
| Completed Bash or `shell` with valid command/output | One command/output block; command may be highlighted, output is ANSI-free plain text, environment is omitted |
| Completed Bash with `exit: null` plus timeout/signal metadata | Preserve output and show the textual timeout/signal status; do not fall through to an empty generic block |
| Completed Edit/Write diagnostics | Keep first three severity-one diagnostics with `range.start`; label `Error [line:column]` using one-based coordinates |
| Any backend truncation flag | Mark the semantic view truncated when top-level or the applicable display/row/group/diff flag is true |
| Pending/running/permission/denied/error/diagnostic | Existing inline or error fallback; no completed block is synthesized |
| Directory or attachment result | Existing directory/attachment presentation; never force file hashline rendering |
| Unknown tool, unknown input key, malformed metadata, wrong row type, or missing required display field | `presentCodingTool` returns `undefined`; generic/error fallback renders bounded safe text |
| Syntax parser/grammar lookup fails | Keep file block and render readable plain text; do not fail the component |
| Backend truncation marker/artifact | Show explicit truncation/artifact status; local collapse must not claim complete content |
| 80-column terminal | Prompt/transcript remain readable; Edit is unified and all blocks clip/wrap without horizontal corruption |
| 140-column (or wider) terminal | Titles and metadata remain visible; Edit may use split layout; Grep groups remain individually titled |
| Session replay or `message.part.updated` | Same projected part replaces the old part and yields the same completed semantic view; no network/timer/Event replay |

### 5. Good / Base / Bad Cases

- Good: a completed Read part with `metadata.display` renders `src/main.rs`,
  starts line numbers at the backend `lineStart`, colors Rust tokens, and
  collapses only the local view. Reopening the Session renders the same part.
- Good: Grep has two groups with different file extensions; each title and
  grammar is independent, a match row is textually marked, and the same groups
  remain readable at 80 and 140 columns.
- Base: a completed Edit is unified at 80 columns, split on a wide terminal;
  hidden `shell` uses the Bash block; malformed metadata uses generic fallback;
  parser failure uses plain text.
- Bad: `fetch()` from a component, a local event reducer, a second tool-result
  store, or an `env` dump. These break replay ownership or expose secrets.
- Bad: render raw metadata keys, infer a file path from arbitrary input,
  highlight Bash output as code, use color without a text status, or choose the
  Edit layout from a hard-coded screen snapshot.

### 6. Tests Required

- `test/coding-tool-presentation.test.ts` must fail on the old inline renderer
  and assert the allowlisted union for each tool, canonical/hidden names,
  valid/invalid metadata, directory/attachment/truncation/error states,
  `env`/unknown-key exclusion, ANSI stripping, and plain-text syntax fallback.
- `test/coding-tool-render.test.tsx` uses OpenTUI `testRender` at exactly 80 and
  140 columns. Assert semantic titles, Read offsets and stable line numbers,
  Edit diff mode/layout, per-file Grep titles/match labels, Bash command-only
  highlighting, readable status text, and `captureSpans()` syntax colors rather
  than a brittle whole-screen snapshot.
- `test/coding-tool-sync.test.tsx` hydrates one SDK part, applies one
  `message.part.updated` replacement, and asserts the rendered part changes
  once. Instrumentation must prove no presentation-specific HTTP request,
  timer, Event replay, or second state owner.
- The real-backend PTY scenario creates Read, Edit, Write, Grep, and Bash
  results at 140 columns, reopens the same Session, and asserts equivalent
  completed semantic blocks. Repeat at 80 columns and assert readable prompt,
  unified diff, titles, status labels, and no leaked environment values.
- Mutation tests should fail if the normalizer accepts an arbitrary metadata
  key, drops the Read offset, changes Grep order, renders ANSI output, exposes
  `env`, removes fallback, or performs a presentation network call.

### 7. Wrong vs Correct

#### Wrong

```tsx
createEffect(async () => {
  const response = await fetch(`/session/${sessionID}/tool/${part.id}`)
  setLocalTool(await response.json())
})

return <text>{part.state.output}</text>
```

This creates a second transport/state owner, bypasses projected replay, and
renders unvalidated output without titles, offsets, semantic metadata, or
secret-field filtering.

#### Correct

```tsx
const view = createMemo(() => presentCodingTool(props.part))

return view() ? (
  <CodingToolPresentation
    view={view()!}
    width={ctx.width}
    diffStyle={ctx.tui.diff_style}
    diffWrapMode={ctx.diffWrapMode()}
  />
) : (
  <GenericTool {...toolprops} />
)
```

The route normalizes one projected part and passes the resulting allowlisted
view to the renderer. SyncProvider supplies live replacement and replay;
malformed or unsupported data remains safe through the existing fallback.

---

## Scenario: Synchronized Agent Model Configuration

### 1. Scope / Trigger

- Trigger: changing bootstrap Agent-model rows, the model picker, Agent target
  selection, model recents/favorites, or backend preference mutation.
- This flow configures every catalog Agent without widening the root-selectable
  `/agents` list.

### 2. Signatures

- `decodeAgentModels(value, providers): AgentModelState[]` owns the unknown-JSON
  boundary in `src/hya/agent-models.ts`.
- `SyncProvider` owns `capabilities.agentModelPreferences`, `agentModels`,
  `getAgentModel`, `refreshAgentModels`, and `setAgentModelPreference`.
- Command: `agent.model.list`, title `Configure agent models`, slash
  `/agent-models`.
- `DialogAgentModels` selects a target; `DialogModel({ agentID })` reuses the
  existing provider/model picker.

### 3. Contracts

- Accept rows only when bounded identities, booleans, mode, source,
  configured/settable consistency, and exact provider-catalog membership are
  valid. Allowlist output fields; never retain unknown or secret-like fields.
- A `remembered` effective row must match its present, available preference.
  A stale preference remains visible but cannot become effective.
- Sync uses the existing SDK `fetch` transport and directory context. No
  component creates another client, poller, preference file, or catalog.
- Replace one synchronized row only after a successful PUT and normalized
  response. On error, keep the old current/recent/synchronized state, show the
  existing toast, and leave the model dialog open.
- Normal `/models`, recent cycling, and favorite cycling persist first for a
  settable current Agent, then update request-local state. CLI/Session hydration
  and variants remain local and do not call the preference route.
- Target options are ordered `Main`, `Subagent`, `System`. Hidden fixed Agents
  are System rows. Configured rows stay visible, show `Configured by Agent
  policy`, and are disabled. Stale state has a text label.
- A targeted selection for another Agent must not mutate the active root Agent.
  `/agents` and Tab/Shift-Tab remain primary-only.

### 4. Validation & Error Matrix

| Input/result | TUI behavior |
| --- | --- |
| Capability absent or not exact `true` | Hide/disable the dedicated flow; preserve old local model behavior |
| Malformed row or inconsistent flags/source | Drop the row at the decoder boundary |
| Unknown response fields | Ignore them; do not copy them into synchronized state |
| Preference absent from provider catalog | Show `stale preference`; use backend effective fallback |
| Configured direct/category row | Show under its group, disabled, with configured text |
| PUT succeeds with one matching normalized row | Replace that row, then close/update local model state |
| PUT fails or returns wrong/malformed Agent row | Preserve prior state, toast the bounded error, keep dialog open |

### 5. Good / Base / Bad Cases

- Good: select a hidden Compaction target, open `Select model for compaction`,
  commit once, and leave the active primary Agent unchanged.
- Base: an old backend has no capability; `/models` keeps its previous
  request-local behavior and `/agents` stays primary-only.
- Bad: write `model.json` for backend defaults, optimistically change local
  model before PUT success, use `agent.list()` as the all-Agent source, or open
  a second model picker implementation.

### 6. Tests Required

- Decoder tests cover malformed/oversized fields, unknown-field exclusion,
  stale preferences, source/flag consistency, and model-local slashes.
- Target-option tests assert deterministic Main/Subagent/System grouping,
  hidden/configured/stale labels, disabled rows, and targeted title.
- Sync/dialog tests assert one PUT, update-after-success, rollback/toast on
  failure, no active-root mutation for another target, and no persistence from
  hydration/CLI/variant state.
- Full Bun type-check/tests and an actual OpenTUI smoke must show the target
  dialog, reused model picker, immediate backend row, and restart restoration.

### 7. Wrong vs Correct

#### Wrong

```typescript
local.model.set(next)
await fetch(`/tui/agent-models/${agentID}`, { method: "PUT", body })
```

The UI claims success before the backend owns the value and cannot roll back
recent/current state reliably.

#### Correct

```typescript
const selected = await local.model.select(next, { recent: true })
if (selected) dialog.clear()
```

`local.model.select` persists through `SyncProvider` first. It updates local
presentation only after the backend returns one valid normalized row.
