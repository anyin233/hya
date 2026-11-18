# ADR-0034: The kernel Context is m, g and w, read through a recorded View

Date: 2026-11-18
Status: Accepted

## Context

ADR-0033's kernel has one part of the Context: m, what the model sees. It is
appended to and only removed from by a Rewrite. Two other parts lived outside
it:

- **g**, the goal as completion criteria, existed only in full mode as the
  `hya/todo-tools` bundle and an in-process `TodoPlane`. After a Rewrite the
  model no longer saw the list.
- **w**, the workspace (files, resources, time), had no record. The kernel
  `write`, `edit` and `bash` tools wrote no `FilesChanged`, and nothing noted
  that a file the model had read was changed by someone else.

The request a round sent was implicitly "the latest projection". Nothing
recorded which state a round had read. Full mode also brought w into requests
outside tool calls: the system prompt re-read every `AGENTS.md` from the
workdir up to `$HOME` and the date on each runtime rebuild.

## Decision

### Decisions

- **C1** The scope is g, an explicit record of w, and the read view, all in
  the `hya-core` kernel, shared by bare and full mode.
- **C2** g is a list of completion-criteria items (`TodoItem`,
  `Event::TodosUpdated`). There is no free-text scratchpad.
- **C3** The model writes g of its own accord; the kernel never generates it.
  It asks once per context epoch, when the window is 60% full, that the model
  bring g up to date (a logged system message), because g is what survives a
  Rewrite.
- **C4** g takes no part in any stop decision, in the kernel loop or in the
  headless `exec` wrap-up (which no longer continues a run for open todos).
- **C5** g is `session.todos`, owned by the kernel. The `hya/todo-tools`
  bundle and the in-process `TodoPlane` are removed.
- **C6** The record of w is the `FilesChanged` of the kernel `write`, `edit`
  and `bash` tools plus a lightweight diff of outside changes. The workspace
  is never hashed.
- **C7** The read view is a first-class value: a request is built from a
  `View`, not from "the latest projection".
- **C8** Rewrite carries g: changes to g stay in m as `todo` tool results,
  and after any Rewrite the current list appears verbatim in m.
- **C9** w is not in the model request. Only m and g go to the provider; w
  enters m only through tool calls.
- **C10** The kernel system prompt keeps `<cwd>`, a one-time description
  given at session start.
- **C11** Tool schemas are the interface to w, not information about w.
- Full mode records its initial description (environment, `AGENTS.md`
  guidance, the directory skill index) once per session as
  `InitialContextRecorded` and reuses it, so editing `AGENTS.md` mid-session
  no longer changes the request.

### Invariants

1. A request is a function of a `View` only. Of the View, only m (which holds
   g as tool results and Rewrite markers) builds the request; w is never
   rendered into it.
2. Every `StepStarted` records its view (`cut`, `request_sha256`). Replaying
   the log to `cut` and building the request with the same harness
   configuration yields the recorded digest.
3. After any Rewrite, when g is non-empty, the current list appears verbatim
   in m.
4. ADR-0033's four invariants still hold. In particular the harness forges no
   tool result, and a Rewrite only removes: the list in its marker restates
   existing state verbatim, like the user messages it already restates.

### Events

| Event | Change |
| --- | --- |
| `StepStarted` | optional `view: {cut, request_sha256}`; absent in old logs |
| `ToolResult` + `TodosUpdated` | written in one batch by the kernel `todo` tool |
| `FilesChanged` | now also written by bare-mode `write`, `edit`, `bash` |
| `FilesObserved` | new: `{session, external, files: [{path, mtime_ns, size}]}`, folded into `session.files`; a deleted file has null `mtime_ns` and `size` |
| `InitialContextRecorded` | new: `{session, text}`, folded into `session.initial_context` (first record wins) |

## Consequences

- The tool names `todo__read`, `todo__update_status` and `todo__update_content`
  are gone; traces after this change are not directly comparable with earlier
  benchmark runs that used them.
- Outside changes are seen only for files the model has read or changed, and
  only by mtime and size. A shell command's Git snapshot attributes an
  outside edit made while the command runs to that command.
- Editing `AGENTS.md` mid-session takes effect only in a new session or when
  the model reads the file with a tool.

See [kernel.md](../kernel.md) and the
[event reference](../architecture/event-model.md).
