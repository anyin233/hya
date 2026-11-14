# ADR-0032: Workflow programs — an agent whose only act is to write a traced program over specialised workers

Date: 2026-11-14
Status: Accepted (experimental bundle `hya-extra/orchestrator`; the engine parts are general)

## Context

The goal is reasoning that is structured, attributable, and efficient: for
any result, one can say which decision led to it, on which evidence, made by
whom, and no step is paid for twice.

A chat-loop agent does not give that. Its plan lives in the model's hidden
state, each tool call is chosen after the previous result, and the only record
is the transcript. Four rounds of experiments with `gpt-6.1-sol` on four task
families showed that a prompt and a tool shape alone do not change this: given
a tool that accepts a branching program, the model wrote straight-line
programs and decided between calls as before (a judge model offered inside the
program was used by 1 of 117 programs).

What did work was inverting control. When the agent has no tool except "submit
a program", the plan has to be in the program, the branch conditions are
expressions over named results, and the runtime that executes it can record
every effect with the source line that caused it.

## Decision

1. **A Workflow program is the unit of reasoning.** It is a JavaScript async
   function body run by a bundle process in a sandbox with exactly these
   effects: `agents.run` (start one worker and wait for its report),
   `models.classify` (typed questions to a discriminative judge model), `text`
   and `console.log` (output), and `args` (input). There is no file, shell,
   network, import, or timer access. Control flow is ordinary JavaScript:
   `if`, loops, `Promise.all`.
2. **The Orchestrator agent can only submit Workflow programs.** Its view
   advertises one tool, `workflow`. It never touches the workspace; in three
   trials the orchestrator session recorded no file change and called nothing
   else.
3. **Workers are specialised by capability, not by instruction.** `scout`
   (read, grep, glob), `runner` (bash, read), `editor` (read, grep, glob,
   write, edit). Only a Workflow program can start them.
4. **Every effect is one trace record**: sequence number, source line, kind,
   input, result, duration, the worker's handle and session, and the
   **Workspace state** before and after. The trace file is the attribution:
   from a changed file to the editor session, to the program line, to the
   observations that line branched on.
5. **Workflow programs are stored by name and reused with new `args`.** A
   program is stored even when its run fails, with the error; `show` returns
   the source so the next run repairs it instead of writing a second one.
6. **A scout's answer on an unchanged work tree is an Observation and is
   remembered.** The key is the Workspace state plus the exact question. A
   reused Observation costs no model call and its trace record names the
   session that made it. Runner and editor answers are never remembered.
7. **A worker nobody can revive ends without a model-written handoff**
   (`handoff: deterministic` on the agent). **A worker is bounded**: 600 s by
   default, archived when it runs out.

The engine gained four general mechanisms for this, none specific to the
bundle:

- **Nested tool calls** (`ToolCtx.nested`, capability operations `tools.list`
  and `tool.call`): a bundle tool can call the other tools of its agent's
  compiled view. The call runs under the calling call's permission snapshot,
  cannot call itself, and its file changes are recorded against the calling
  call.
- **`advertise`** in a resource view: which of the allowed tools the model is
  shown. The orchestrator is allowed the coordination tools so that its
  program can use them, and shown none of them.
- **`handoff: model | deterministic`** per agent.
- **`workspace.state`**: a SHA-256 identity of the Git work tree containing
  the session's working directory (location, HEAD tree, status, and the
  content of every status-listed file).

## Decisions taken against the first design

- *Remember every worker answer that left the workspace unchanged.* Rejected
  at review. A command can give a different result on the same files: ignored
  files, the environment, chance. One trial task repeats a command on purpose
  to expose a flaky test, and a cache would have answered "passes" five
  times. Only a scout, which can only read, is remembered.
- *Content-only workspace identity, shared between checkouts.* Rejected. An
  answer can name absolute paths; reused in another checkout it would send an
  editor to the wrong tree.
- *One general worker.* Rejected after trial 1: with a general worker the
  program degenerated to "delegate the whole task" (one worker per run, 71 of
  111 programs only fetched).
- *Let a failed program vanish.* Rejected: the second run then writes a new
  program for the same kind of task and the store fills with near-duplicates.

## Consequences

Measured on twelve task variants (four synthetic task families and one task
in this repository), each run twice with `gpt-6.1-sol` at medium effort; the
second pass starts with the stored programs and Observations of the first.
Wall time is on a shared machine.

| | runs that succeed | median wall time | prompt tokens per 12 runs |
| --- | --- | --- | --- |
| chat-loop agent, default tools | 24 / 24 | 40 s | 1.28 M |
| first version (one handoff per worker, no reuse of answers) | 24 / 24 | 266 s | 3.46 M |
| this design, first pass | 12 / 12 | 151 s | 3.46 M |
| this design, second pass | 12 / 12 | 127 s | 2.30 M |

- In all runs the orchestrator called only `workflow`, changed no file, and
  no worker used a tool outside its role.
- Every program of the second pass was a stored one (12 of 12); in the first
  pass 6 of 12 runs already reused a program stored by an earlier variant.
  All 26 programs branch, 25 loop, 24 start workers in parallel.
- 11 scout answers were reused in the second pass; each names its source.
- A worker takes a median of about 30 s (50 to 64 s before the deterministic
  handoff). A warm run is three workers in sequence (baseline run, edit,
  verification run) plus about 16 s of the orchestrator's own model time.
- The price of attribution stays: a warm run is about three times as slow as
  the chat loop and uses about 1.8 times the prompt tokens. The targets set
  before the trial were 160 s cold (met), 106 s warm (missed), and 2.42 M
  tokens per pass (missed cold, met warm).
- An earlier trial of the same build without the worker bound lost 6 of 24
  runs to one cause: a scout searched `/` for `AGENTS.md`, the call never
  returned, and the program waited for it. Hence decision 7.

Known limits, by decision:

- A Workflow program is not replayable: effects are recorded, not re-applied.
- The trace is a file under the bundle's config directory, not part of the
  event log. The worker sessions and file changes it names are in the event
  log, so the link is by session and call id.
- A scout that reads files Git ignores (build output, logs) must pass
  `cache: false`; the Workspace state does not cover ignored files.
- An editor is started fresh for each repair round.
- The discriminative judge is available and unused: over three trials no
  program called it. Branch conditions were computed from the typed `result`
  fields that workers return, which made a second classifier unnecessary on
  these tasks.
- The base `glob` tool walks the whole tree under its path for a pattern
  without `**` and has no bound of its own. The worker limit contains it; the
  tool itself is unchanged.
