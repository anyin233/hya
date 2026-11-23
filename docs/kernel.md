# Three-primitive kernel

`hya exec --bare` runs the `hya-core::kernel` loop alone with six built-in
tools, a minimal system prompt, and the SQLite event log. It loads no bundle.
Use it to run a task with only the kernel's Context operations. See
[ADR-0033](adr/0033-three-primitive-kernel.md) for the design and invariants.

```sh
hya exec --bare "say hi" --db /tmp/x.db
```

The working directory is the caller's cwd. `--bare` conflicts with `--pure`
and always runs in process. Add `--time-budget <SECS>` to
announce the budget to the model, report its progress, send WrapUp and Final
guidance, and cap Bash calls. See the [CLI reference](cli.md#--bare).

## Three primitives

The kernel folds the event log with the shared `hya-proto` reducer.
The model sees the prompt, tool schemas, and live transcript (`m`).
The session also tracks completion criteria (`g`) as an ordered todo
list. The model sees that list in tool results and Rewrite markers.
Three primitives update Context.

| Primitive | Meaning | Events |
| --- | --- | --- |
| Model append | The model's text, reasoning, and tool calls | `StepStarted`, `TextStart/Delta/End`, `ReasoningStart/Delta/End`, `ToolCallRequested`, `StepFinished`, `UsageRecorded`, inside assistant `MessageStarted`/`MessageFinished` |
| Tool append (tool) | An observation paired with a model call, including changes to the session todo list and observed workspace files | `ToolResult`, `ToolError`, `TodosUpdated`, `FilesChanged`, `FilesObserved` |
| Tool append (user) | User input through the same observation channel | user `MessageStarted`, `TextStart/Delta/End`, `MessageFinished` |
| Rewrite | Remove earlier Context while retaining its log | a system marker message and `ContextCompacted` with strategy `rewrite` |

The log only grows. Model text remains a claim; only tool append introduces
outside observations. Unknown tools, invalid input (including argument text
that is not valid JSON), execution errors, and cancellation return as failed
tool results. Tools run sequentially, with no retry, model fallback, or step
limit.

### Tool calls whose arguments are not valid JSON

A model streams a tool call's arguments as text. When that text is not valid
JSON (cut off inside a string, or followed by extra text), the provider
decoder still records the call as `ToolCallRequested`, with the marker input
`{"invalid_json_arguments": {"text": <the raw argument text>, "error": <the
parser error>}}`, and the round ends normally. The kernel loop checks every
call before it runs it, in both modes, ahead of `rewrite`, `todo`, the
`ToolGate`, hooks, and permission checks. For a marker input, no tool runs.
The call gets a `ToolError` with this text, and the next round goes on:

```text
arguments are not valid JSON: trailing characters at line 1 column 829. Nothing was run; call the tool again with its arguments as one valid JSON object.
```

The marker stays the call's recorded input. Later requests send it back as
the call's arguments, an object every provider accepts, and the event log
keeps the raw text and the parser error for analysis. Valid JSON, including a
value that is not an object, reaches the tool unchanged. Empty argument text
is still a decode error that fails the round. A model that sends the marker
shape itself as its arguments is rejected the same way.

The same rule covers a stream that ends inside a call's arguments when the
decoder still closes the call: at the output limit (`length`), or when the
stream ends cleanly without a terminal event on the Chat Completions and
Anthropic routes. A connection that breaks mid-stream fails the round as an
interrupted stream, as before: the decoder never closes the call it was
streaming, so that call is not recorded.

## Request views

Each round reads a `kernel::View` after the harness has had a chance to
rewrite the context. The view borrows the folded session projection.
`View::cut()` gives the last durable event sequence in that projection,
inclusive. `Harness::request(&View)` takes this view and supplies the
resolved harness inputs needed to build the request.

In a budgeted bare run, the kernel checks the clock before each model
round. If the budget has crossed another tenth, it logs
`HEADLESS RUN — clock: Xs of Ys used, Zs left.` as a system message.
A new WrapUp or Final notice takes precedence. These notices are sent as
user-role messages in the conversation. A stage notice or a clock line
leaves the request after the assistant responds, so each is the last
message of exactly one request and the earlier history is left as it was.
Tool results keep their original payloads.

The lead agent’s Final notice includes this instruction to preserve a checked
deliverable as the time limit approaches.

> If a delivered file already passes your checks, do not edit it in place — test
> any change on a copy first and replace the file only if the copy still passes.
> If a delivered file is unverified, verify it now and fix it directly only if
> it fails.

### Dropped-reasoning notice

Some model endpoints silently leave a step's replayed reasoning out of the next
request's prompt (see [OpenAI Responses
protocol](architecture/providers.md#openai-responses-protocol)). The model then loses whatever it
kept only in private thinking — derived values, findings, plans — at every
tool call. In both bare and full mode the kernel detects this from provider
usage and tells the model once per turn to keep that state in visible text, a
workspace file, or the todo list.

Detection compares each round with the previous round of the same run
(`PrevRound`). The model that served the round decides everything route
specific — what was replayed, whether images were attached — so a full-mode
fallback model is judged by its own route, not by the requested model's. Full
mode looks that route up on the engine's live provider router
(`Harness::served_route`), since a route can be added mid-turn; bare mode uses
the run's router. The previous round must have made tool calls and reported at
least `MIN_DROPPED_REASONING_TOKENS` (1,000) reasoning tokens with a known split
(`reasoning_unknown` false), the serving route's `ReasoningReplayPolicy` must
have `expands_next_prompt()` true (`ProviderData` and `ReasoningContent`), and
the current request must replay at least one reasoning part from that round via
`ReasoningReplayPolicy::replays`. Both rounds must report a non-zero prompt
(`input + cache_read + cache_write`). The next prompt holds the previous one
plus that round's output, reasoning included, so a drop is recorded when

```text
curr.prompt < prev.prompt + prev.reasoning / 2
```

Gating on `expands_next_prompt()` alongside `replays` keeps the check off
routes that do not bill replayed reasoning back into `prompt`: routes with
`ReasoningReplayPolicy::None` replay nothing, a chat route that reports
reasoning tokens without streaming `reasoning_content` has no part to replay,
`GoogleThought` routes replay compact encrypted `thoughtSignature` tokens
without re-expanding `promptTokenCount` by prior `thoughtsTokenCount`, and
`SignedCurrentTurn` (Anthropic) reports no thinking split
(`reasoning_unknown`).

A pair is not compared when either request legitimately changes or shortens
earlier history:

- the serving model changed;
- a rewrite or compaction moved the compaction marker, or tool-output eviction
  changed how many sent tool outputs (`sent_parts`, so folded parts do not
  count) carry an eviction notice;
- either request attaches a tool-result image (sent once; the next request
  carries a text note instead);
- either request ends with a one-request notice: a goal checkpoint, a
  deadline stage or clock line, the completion check, the empty-turn notice,
  the [reasoning-only deadline notice](#reasoning-only-deadline-guard), or
  this notice itself;
- the previous round made no tool calls (a continuation follows it).

A detected drop logs the warning `reasoning_replay_dropped` (session, step,
`prev_prompt`, `curr_prompt`, `prev_reasoning`). If the round made tool calls
and no request of this run has delivered the notice yet, the kernel appends
after those calls a system message, delivered as the last user message of the
next request only, in both modes:

> [hya] Reasoning replay notice: the model endpoint did not carry your
> previous step's private reasoning into the next request. Do not rely on
> private thinking being available in later steps: keep key derivations,
> findings, decisions, and next steps in brief visible text alongside your
> tool calls, in a workspace file, or in the todo list.

"Once" counts delivery, not logging: the notice counts as sent when a request
carries the notice message the kernel appended (matched by its id, so a user
prompt quoting the notice text does not count), and a rewrite before the next
request that removes the notice unsent leaves the next drop free to send it. The next
round's parts take the notice out of later requests. If that round has no
parts at all and the run continues with the empty-turn notice or the
completion check, the continuation replaces the notice instead of joining it.

The two kinds of loss are found differently. A Responses endpoint that binds
each reasoning item to the history before it removes the item when that
history changed in place, and reports it (`prefix_mismatch` in
`/response/input_transformations`); the decoder logs those reports. hya's own
in-place changes — a tool-result image replaced by its note, a tool output
replaced by its eviction notice, a rewrite, a transient notice removed — are
exactly the rounds the guards above skip, so the usage check does not count
such removals. A gateway that drops every replayed reasoning item even when
the history only grew reports nothing; the usage check finds it on those
append-only rounds.

Each `StepStarted` records a `view` with the numeric `cut` and a
`request_sha256` string. The digest covers the harness's completed system
prompt, tool schemas, and messages. Retries reuse the same view reference.
See the event reference for the digest encoding and optional field.
[Step markers](architecture/event-model.md#step-markers-provider-rounds)

To verify a bare-mode log offline, run the ignored replay test from the
repository root with the source revision used to record the log:

```sh
HYA_BARE_REPLAY_DB=/path/to/hya.db \
  cargo test -p hya-core --test it replay_bare:: -- --ignored --nocapture
```

The test opens the database read-only. It rebuilds each recorded bare-mode
request from the session's model, working directory, log prefix, and the
kernel's fixed prompt and tools. It prints the equal and unequal counts
and fails on any unequal view. Full-mode replay also needs the resolved
round configuration, any referenced attachment blobs, and reproducible
hook output. Legacy URI media entries can acquire fresh part IDs during
request assembly.

### Reasoning-only deadline guard

With `--time-budget`, deadline notices otherwise reach the model only between
rounds (bare mode) or on a tool result (full mode). A model step that streams
nothing but reasoning has no such point: it could think past WrapUp and Final
until the process is killed, without ever reading a notice. The kernel
interrupts a reasoning-only stream when its per-step wall-clock cap or the
next stage checkpoint comes due, then continues the turn with a notice.

The guard applies in bare and full mode alike whenever the run has a deadline
(`hya_tool::deadline::current()`); there is nothing to configure. A round
arms it once, on its first non-empty `ReasoningDelta` before any text or tool
call. Waiting for the first byte (request prefill) is never cut, nor is a
stream that never reasons.

- It waits for the next stage the session has not been told of: WrapUp, or
  Final once WrapUp was told (or when the budget is too short for a WrapUp
  window), but at least the reasoning grace, `delay.max(reasoning_grace())`.
  The grace (3 % of the budget, 15–30 s, and at most half of what is left
  before `Over`; 27 s for a 900 s budget) lets a short step that starts just
  before a stage, or after a stage the session was not yet told of, finish
  its thought and act; in full mode its tool result then carries the stage
  notice. A step also has a cap of 30% of its remaining budget at the first
  reasoning delta, clamped to 90–420 seconds and at least the grace.
  Whichever comes first interrupts the step. For a fresh 900-second run the
  cap is 270 seconds. A cut before a stage sends the clock and asks for a
  concrete tool call. The stage remains due at its usual time.
- Once armed, the guard is checked ahead of the stream (after a cancel), so
  buffered stream chunks cannot hold off a checkpoint that has passed.
- The first visible text (`TextStart` or `TextDelta`) or tool call
  (`ToolInputStart` or `ToolCallRequested`) disarms it for the rest of the
  round, so an answer or a file write in progress is never cut.
- Once the session was told of Final it is not armed again: the model's last
  step runs to its end.
- The step's own end (`MessageFinished`) disarms it, even while the stream
  stays open until EOF, so a finished step keeps its reported finish.

A cut is not a cancel. The round keeps the reasoning it streamed (closed parts
and the open one, persisted as on any stream end), its step finishes as
`stop`, and the next round of the same turn starts with a system message.
The kernel appends it after that round's rewrite (`Harness::before_round`),
so a compaction marker the rewrite creates precedes it and cannot hide it
from the request. That holds also when a full-mode native compaction folds
the turn's own assistant message behind the marker: the notice stays in the
request until that message records its next part:

> [hya] Reasoning interrupted at budget checkpoint: the run reached a deadline
> stage while your last step was still thinking, before any answer or tool
> call, so that step was stopped. Do not restart the deliberation: act on what
> you have worked out, with the next tool call or your final answer.

A stage cut includes the stage’s `HEADLESS RUN — TIME` or `FINAL` notice,
with the `report` wording for a subagent. The stage counts as told, so the
next round can still announce the next stage when it comes due.

A per-step cap reached before a new stage sends the current clock with this
notice:

> [hya] Reasoning interrupted at budget checkpoint: your thinking-only step
> reached its wall-clock cap and was stopped to keep the run moving. Act on
> what you worked out with a concrete tool call now.

Both cuts include the reasoning under “Where your thinking stopped”: all of
it through 3,200 characters, otherwise the first 1,200 and last 2,000.
The next request carries the notice as a user message. An end continuation
after a partless round replaces the notice.

The stopped step's reasoning stays in the transcript, but a reasoning block
of that step that had already closed loses its replay data (a second
`ReasoningEnd` with no `provider_data`, which clients see as a repeated
`partCompleted`). Nothing of the step followed that block, and a route that
replays reasoning would otherwise send it before a later step's direct tool
call or answer. On the OpenAI Responses route, the encoder also leaves out of
`input` any encrypted reasoning item that no answer or call follows, since the
API rejects such an item
([Encrypted-reasoning replay](architecture/providers.md#encrypted-reasoning-replay)).

Interfaces:

- `RunDeadline::next_checkpoint(session) -> Option<(Duration, Stage)>`
  (`hya-tool`): time until the next untold stage (zero once it has started),
  and that stage; `None` once the session was told of Final. `RunDeadline`
  reads Tokio's clock, so a paused test runtime moves its stages.
- `RunDeadline::reasoning_grace() -> Duration` (`hya-tool`):
  `clamp(0.03 × budget, 15 s, 30 s).min((remaining − grace) / 2)`, the least
  time a reasoning step gets before a cut.
- `Round::deadline_cut: Option<DeadlineCut>` (`hya-core`): set when
  `round::collect` stopped the stream; `DeadlineCut` carries the stage reached
  (`WrapUp` or `Final`) and the reasoning streamed before the cut.
- `Harness::deadline_subagent() -> Result<bool, CoreError>` (`hya-core`):
  whether the stage notice takes the `report` text. Bare mode answers `false`;
  full mode's `TurnHarness` uses the same check as its tool-result notices.
- `hya_proto::context::REASONING_DEADLINE_NOTICE_PREFIX`: the notice's opening
  line. `is_harness_transient_notice_text` recognises it, so the notice does
  not start a new turn, and `request.rs` sends the logged system message as a
  user message until the model's next part.

## Built-in tools

The following JSON shapes use `string` and `int` as type placeholders. All
input fields are required except `bash`'s `timeout`. Relative paths resolve
against the run's working directory.

In full mode `hya/base-tools` supplies `read`, `write`, `edit`, and `bash`
under these names with richer inputs ([Differences from full
mode](#differences-from-full-mode)); the kernel's prompt lines for them hold
in both modes.

| Tool | Input JSON | Success result |
| --- | --- | --- |
| `bash` | `{"command": string, "timeout"?: int}` | `{"exit_code": int, "output": string}`; when the wait ends first `{"exit_code": null, "output": string, "still_running": string}`, or `"timed_out": string` in place of `still_running` when the run's budget cut the wait (the command is killed) or has no time left. The last line of `output` is `[exit code: N]`, `[still running: …]`, `[stopped: …]`, or `[not started: …]`. |
| `read` | `{"path": string}` | UTF-8 text verbatim as `{"output": string}` or an image as `{"output": string, "attachments": [{"type": "file", "mime": string, "url": string}]}` or `{"output": string}` when the image exceeds the attachment limit |
| `write` | `{"path": string, "content": string}` | `{"bytes": int}` |
| `edit` | `{"path": string, "old": string, "new": string}` | `{"replaced": 1}` |
| `rewrite` | `{"summary": string}` | `{"removed_messages": <n>, "note": "The summary in this call is your own text, not an observation."}` |
| `todo` | `{"ops": [operation, ...]}` | Full list as a JSON string. See [Session todos](#session-todos). |

Bare `read` limits each serialized image attachment to 2 MiB, including
base64 and the attachment array. For example, reading a 2 MiB PNG returns
only `output` with the MIME type, file size in bytes, and the file size
limit (about 1.5 MiB, depending on the MIME type). The message says the
image is too large to attach and tells the model to downscale or crop it
with a program and read the smaller file.

`bash` runs `bash -c` when it finds a Bash executable on `PATH` and uses
`sh -c` otherwise. It selects the shell once per process. The command runs in
the working directory with stdin connected to `/dev/null` and stdout and
stderr merged. The tool passes only `-c` as a shell option and uses the exit
code reported by the shell. A nonzero exit is still a success result for the
model to interpret. Failure to start returns `ToolError`.

`timeout` sets the requested wait in seconds, with a default of 300.
Values outside 1–86400 are an input error. With `--time-budget`, the call
waits at most the smaller of that wait and one tenth of the run's budget,
subject to the end-of-budget cap below. While it waits, the model cannot
act or see time updates. A 900-second budget limits one wait to 90 seconds
and a 1,800-second budget limits it to 180 seconds. With a 3,600-second
budget the default stays 300 seconds and a requested 600-second wait is
capped at 360 seconds. Without a budget the requested or default wait applies.

Because tool results are sent again with every later request, when Bash has
captured more than 30,000 bytes at the end of its wait, `output` shows the
first 10,000 and last 20,000 bytes with a marker line giving the omitted
and total byte counts and the quoted path to the retained file holding
the complete output. The model receives only the result's `output` string,
so Bash appends one status line after the captured output, adding a newline
when needed.

Each call creates a private directory (`mkdtemp`, `hya-bash-XXXXXX` in the
system temporary directory) holding an `output` file, which receives the
command's stdout and stderr, and an `exit_code` file. Commands run with
`PYTHONUNBUFFERED=1`, so Python output reaches the file as it is printed,
while other programs that buffer output when writing to a file may still
show it late.

A call ends in one of four ways:

- **The command ends within the wait.** The result is
  `{"exit_code": int, "output": string}` with the captured output followed by
  `[exit code: N]`, including `[exit code: 0]` on success. A command killed
  by a signal reports `128 + signal`. Bash removes the call's directory
  when the captured output fits the limit.
  With a time budget, Bash waits for the original process group. A job that
  detaches with `setsid()` can keep its file descriptors open after the call
  returns. Without a budget,
  a job the command started with `&` keeps running when its shell exits.
  Redirect its output to a file of its own to keep it when Bash removes
  the directory.
- **The wait ends first** (the call's `timeout`, the default, or a tenth
  of the run's budget). The command keeps running. The result has
  `exit_code: null`, the output printed until the wait ended, and a
  `still_running` sentence that names how long the call waited and which
  limit that was, the command's process group, the `output` and `exit_code` files,
  how to follow it (`tail`, `cat`, with the paths single-quoted for the
  shell), wait again (sleep briefly in a later call before reading the
  output), or stop it (`kill -- -<pgid>`). The running `hya` process writes
  its exit code (or `128 + signal`) to `exit_code` when it finishes.
  Yielded groups survive across calls. When the run ends, is cancelled, or
  reaches its deadline, Bash kills any remaining yielded group and writes
  `137` to `exit_code` before exit. The last line of `output` is
  `[still running: …]` with the `still_running` sentence.
- **The run's budget cuts the wait short.** This happens when the
  end-of-budget cap is shorter than the wait above. If the command is
  still running at that cap, Bash kills its process group.
  The result has `exit_code: null`, the captured `output`, and a `timed_out`
  sentence naming the budget. The last line of `output` is `[stopped: …]`
  with that sentence. Bash removes the call's directory when the captured
  output fits the limit. Once the budget has no time left, Bash skips
  starting the command and returns a `timed_out` result whose `output` is
  just `[not started: …]` with the reason.
  See the [CLI reference](cli.md#--bare).
- **The run is cancelled.** Bash kills the command's process group and the
  call fails as cancelled. The same cleanup runs when Bash cannot read the
  `output` file: the call fails and no command is left running unreported.

Bash kills the process group only in those last two cases. Waiting without
killing lets the model get control back after a slow command without
destroying the command's work: an interrupted package installation can leave
its package database broken, and a killed build loses its progress. Only a
run that is ending kills, because nothing could follow the command up.

```json
{"command": "make -j8 2>&1 | tail -20", "timeout": 1200}
```

Use `read` with `{"path":"notes.txt"}` to read a UTF-8 text file. The result
contains only `output`, and the model receives the file's text verbatim.
For an image, use `{"path":"diagram.png"}`. `read` recognizes PNG, JPEG, GIF,
and WebP by their file signatures. It returns a description in `output` and a
base64 data URL in `attachments`. Supported provider routes send the attachment
to image-capable models at most once, in the request after the step that read it:
every image costs time and tokens on each request that carries it, so later
requests carry a notice that tells the model to read the file again to see it
again (see [Tool-result images](architecture/providers.md#tool-result-images)).
For an image above the attachment limit, `output` contains resize guidance and
is the only field. If an image within the limit fails to decode, the successful
result contains only `output` with its MIME type, size, and the decoder error.
Missing files and invalid UTF-8 in non-image files return
`ToolError`.
`write` creates parent directories and returns the byte count; I/O errors
return `ToolError`. `edit` requires exactly one occurrence of `old`; zero or
multiple matches return `ToolError` stating the count. Identical `old` and `new`
return `ToolError::Input` without writing the file.

`rewrite` keeps its `summary` in the call input in the current step. It does
not store a second copy or turn the summary into an observation. `<n>` is the
number of removed messages. With no earlier content it returns
`ToolError("nothing to remove")` and emits no `ContextCompacted`.

## Session todos

Use `todo` in either mode to record checkable completion criteria and any results
that will be needed after earlier context is removed. Update statuses as they
change and leave correct items as written. The model chooses when to use the
tool, and the kernel executes it directly like `rewrite`. Open todos do not
keep the kernel loop or headless `exec` running.

When the request would fill 60% of the model window, by the same measure as
the [rewrite trigger](#rewrite-rules), and no reduction ran before the round,
the kernel can append one logged system message per turn starting with
`[hya] Goal checkpoint:`. It asks the model to record
in the list every result it has derived and still needs. The request
includes the notice until the assistant produces another part or a rewrite
occurs. If a rewrite happens first, its marker carries the list or asks for
one. A marker that carries the list first says its recorded results stay valid
and asks for further results to be recorded as they are obtained. The event log and its replayed View determine which requests show the
checkpoint.

| Operation | JSON fields | Effect |
| --- | --- | --- |
| Add | `op: "add"`, `content: string`, optional `status: string` | Append an item with a fresh decimal string id. Status defaults to `pending`. |
| Edit or status | `op: "edit"` or `op: "status"`, `id: string`, and `content: string`, `status: string`, or both | Update the provided fields. |
| Remove | `op: "remove"`, `id: string` | Remove the item. |

The allowed statuses are `pending`, `in_progress`, `blocked`, and `completed`.
The canonical schema requires an explicit `op` from the table. Callers that
bypass schema validation can also use the decoder’s compatibility forms.
When `op` is omitted, a `content` field without an `id` adds an item. An `id`
with `content`, `status`, or both updates that item. With an `id` and no
`status` field, a status in `op` also updates the item. For example,
`{"ops":[{"op":"completed","id":"#1"}]}` marks item 1 complete.

Operations apply in order. This call adds a criterion with an initial status.

```json
{"ops":[{"op":"add","content":"Focused tests pass","status":"in_progress"}]}
```

The result string is `Current todos:\n- #1 [in_progress] Focused tests pass`.
Each additional item appears on its own row in list order. An empty list is
`Current todos: (empty)`. Pass `{"ops":[]}` to read the current list. An id may be
given as rendered (`#1`). Blank content, an unknown item id, or a malformed
operation fails the whole batch with `ToolError`. A changed list is appended as
a `TodosUpdated` full snapshot in the same batch as its `ToolResult`. Empty and
unchanged lists can be read without a new update. The existing session todo API
reads the replayed list.

## Workspace file observations

Bare and full mode maintain the workspace part of Context (W) in the event
log. On local Linux the kernel watches the session's working directory with
Rust `notify` and inotify. Sessions of one engine share a watcher for each
canonical root and advance independently. The first model round inventories
regular files, including files no tool has opened. Subsequent rounds reconcile
changed paths and the descendants of directories moved into or out of the root.
A quiet round advances its view without a file observation event.

This is automatic in an ordinary `hya exec --bare` run and in `hya serve`.
For example, open two sessions in the same directory, run a turn in each, then
create `report.md` from another terminal. At the next model round in each
session, W records `report.md` before `StepStarted.view.cut`. Each agent can
read the file to obtain its contents. The inventory stays in the projection
and adds no file list or content to the provider prompt. A library host running
repeated bare turns can share one `kernel::WorkspaceWatches` with
`kernel::run_with_watches`; `kernel::run` owns a watcher for its run.

The event contract is `FilesObserved { session, external, files }`. Each file
is `{ path: string, mtime_ns: i64 | null, size: u64 | null }` with an absolute
path, Unix nanoseconds and byte size. Absence produces two nulls for a known
path. Replay replaces that path's entry in `session.files` and counts every
`FilesObserved` and `FilesChanged` event as the session's file revision. The
[`workspace.state`](plugin-protocol.md#request-scoped-host-capabilities) host
capability reconciles the watch outside a round and returns that revision,
so a bundle tool can tell whether W changed. An event can
invalidate an unchanged stamp, for example an equal-length rewrite that
restores the old modification time. Consumers tracking freshness should
follow the event sequence. Notifications coalesce intermediate operations. A transient file that vanishes
before reconciliation may leave no observable state.

Tool observations carry `external: false`. Bare mode stamps explicit read,
write and edit targets. Full mode also handles patch targets, LSP's explicit
file and grep results, including nested calls. With an active baseline, a
shell call samples changes at completion, including newly created files in
a non-Git workspace. A pre-request observation carries `external: true`.
These flags identify observation boundaries. Concurrent changes during a
shell command can be attributed to that command. Revert still requires the
separate pre-change content captured by `FilesChanged`.

The next round after a daemon restart, root replacement, lost notification
stream or expired reader history reconciles durable W with an inventory.
It invalidates surviving files and marks known removals. A file created and
removed entirely while observation was interrupted leaves no evidence.
Automatically discovered names must be UTF-8. Directory symlinks, Git
internals, workspace scratch and output directories (`.hya/local` and
`.hya/tool-output`), and the active SQLite/runtime files are excluded from
discovery. Project files such as `.hya/skills` remain in scope. Explicitly observed paths outside the watched scope
and symlink targets retain metadata checks.

An engine caches 16 recent roots and retains any roots currently in use. Directory registration is subject
to the operating system's inotify quota. The change journal holds up to
8,192 distinct dirty names and 256
reader cursors per root. Exceeding the history limits causes an inventory on a
reader's next round. If a native watch cannot be installed, the kernel logs
the failure and checks the session's known file stamps; setup is retried after
30 seconds. Other operating systems use the known-file check. That check can
detect ordinary changes to known paths but cannot discover new files or detect
an equal-size write with the original mtime.

See the [event reference](architecture/event-model.md#file-snapshots-and-revert)
and the [revert documentation](architecture/runtime.md#file-snapshots-and-revert).

## Rewrite rules

A rewrite removes everything before the current step: earlier messages and
earlier steps in the current assistant message. It keeps the current turn's
user message (`StepCut.kept_head`) and the current step, including its
reasoning, calls, and results. It removes whole steps, leaving no dangling
call/result pairs and never removing reasoning on its own.

The harness writes a system marker message and `ContextCompacted` in one
batch. The marker text is:

```text
<COMPACT_CONTEXT_MARKER>[hya] Earlier context was removed by the <model|harness>. It stays in the session log and cannot be read back.
User messages from the removed part, verbatim:

<each removed user message's text, in order, separated by "--- user message N ---">
```

`<model|harness>` names the initiator. User text is repeated verbatim; the
model cannot replace it with a summary. When the session has todos, the
marker appends a blank line followed by the current list exactly as rendered
by the tool. A `remote` fold's marker carries the same list. The `shake`
step preserves the most recent visible `todo` result when it evicts stale
tool outputs. Removed originals stay in SQLite, but the kernel provides no
way to read them back into Context.

`ContextCompacted` records `strategy: Rewrite` (wire name `rewrite`),
`from_message` (first visible message), `to_message` (current assistant
message), `folded_count`, `input_tokens_est`, `threshold`, and
`step_cut: Some(StepCut { through_part, kept_head })`. `through_part` is the
last part before the current step; `kept_head` is the current turn's user
message. If the assistant message has no earlier step, `step_cut` is `None`
and `to_message` is the preceding message.

Before a round, `kernel::before_round` measures the request:

```text
used      = max(last.prompt() + last.output,
                usage-anchored count of the request + overhead,
                local estimate of the request + overhead)
threshold = max_context * 8 / 10
```

`last` is the last assistant message's `usage.last_round`; `overhead` is the
system prompt and offered tool schemas. The request counts include tool
results that arrived after the last round. A completed tool result is counted
as it is sent. The estimate uses its string output, or the serialized JSON
when there is no string output. It charges 5,000 tokens for each prompt image
and each tool image the replay plan attaches. A tool image is planned only for
the request after the round that read it, up to 2 MiB per round in call order
(the first always); the encoders follow the same plan, but an image the
encoder's media check rejects is still charged. For every other image, and
when a route cannot send tool images, it counts the text notice that replaces
it.
The estimate also counts the notice when the tool-result size cap drops an
attachment. The provider validates image data when encoding the request.

When `used > threshold`, the kernel runs its extensions (none in bare mode),
re-estimates the request from the re-read projection when one changed the log,
and rewrites when the request is still over the threshold. Without an
advertised window, an oversized-request provider error surfaces unchanged. Harness rewrites use the same removal range as model rewrites, and
record `used` and `threshold` as `input_tokens_est` and `threshold`; a model
rewrite records the last round's usage. See
[Compaction](compaction.md#before-each-round) for the full sequence.

Known limitation: image attachments in removed user messages are not kept
with the repeated text.

## System prompt

`kernel::prompt::system_prompt(cwd)` returns the following text, with `{cwd}`
replaced by the working directory:

```text
You are a coding assistant running inside hya, a coding agent harness. Help users inspect files, run commands, and modify code.

<tools>
- read: read a text file or image
- bash: run a shell command
- edit: replace text in a file
- write: create or overwrite a file
- rewrite: remove everything before this call from the conversation, keeping the summary you pass
- todo: record the task's completion criteria as checkable end states and update each one's status when it changes; the current list survives rewrites
</tools>

<rules>
- Use bash for file operations like ls, rg, find
- Only tool results tell you what is true outside this conversation. Your own text is a plan or a claim until a tool result supports it.
- Before reporting that something works, run the check and read its result.
- Check the final deliverable itself against the most direct evidence the task gives: the source it was copied or merged from, the literal wording it requires, a stated value, prefix, or dimension. A check built on the same assumption as the thing it checks proves nothing.
- When a tool result contradicts your result, resolve the contradiction before doing anything else.
- Stay inside the workspace and environment the task gives you. Do not inspect harness internals, credentials, or unrelated host/container metadata, do not scan, probe, or try to log in to the host machine, other machines, or services outside the task, and do not try to escape a container or sandbox. If the task cannot be finished without that, say so and stop there.
- Before keeping or replacing a result, verify what each check actually tests. A passing self-written test does not prove an assumed formula or convention; when two interpretations disagree, run an independent check against the given materials or ground truth to decide.
- A rewrite summary is your own text, not an observation. Put in it what a later correction would need: the task, what was checked and how, what failed and why, what is still open.
- Be concise in your responses
- Show file paths clearly when working with files
</rules>

<cwd>
{cwd}
</cwd>
```

The environment rule keeps a stuck agent from turning to the systems around its
task for a way forward.

The prompt is composable: `kernel::prompt::compose(cwd, offered, extra,
file_operations)` builds it for a round from

| Input | Type | Effect |
| --- | --- | --- |
| `cwd` | `&Path` | The `<cwd>` block |
| `offered` | `Fn(&str) -> bool` | Which tool names the round offers. `<tools>` lists the kernel's line for each offered kernel tool, in the order above, whoever implements it. |
| `extra` | `&[(&str, &str)]` | Name and line of tools the kernel does not know, listed after the kernel's lines in the given order |
| `file_operations` | `Option<&str>` | Replaces the rule `Use bash for file operations like ls, rg, find`. Without it that rule is kept only when `bash` is offered. |

`system_prompt(cwd)` is `compose` with all six kernel tools and nothing else;
its text above is pinned by a test.

Full mode's system prompt starts with this prompt, composed from the tools
the round's request offers. The kernel's lines cover `read`, `write`, `edit`,
`bash`, `rewrite`, and `todo`; a tool-family tool adds the line its
`exposure.yaml` declares (`prompt:`), and `hya/base-tools` declares the
file-operations rule used when the round offers `ls`, `glob`, `find`, or
`grep` ([Tool-family presets](base-tools.md#interface-definitions)). MCP and
plugin tools get no line; their schemas describe them. With an exclusive
tool in effect (code mode) the list follows the schemas actually shown. The
agent's prompt, the session's recorded initial context
([Project context](configuration.md#project-context-agentsmd)), and the team
quick reference follow, in that order, separated by blank lines. Every
agent whose turn runs on the kernel loop gets it, bundle agents included;
fixed system agents that make one-off completions without a turn (such as
the title generator) keep their own prompts. For example, `hya-scout`, which has no
`bash`, gets no `bash` line and the base-tools rule instead of the bash rule;
a GPT-family model also gets the `apply_patch` line.

## How a bare run ends

A bare run ends when a model round answers without a tool call, with two
exceptions: a turn that gives no answer at all (see [Turns without an
answer](#turns-without-an-answer) below), and the completion check. The
first answer gets one completion check before the run ends: the harness appends the logged system message below, in the same form
as the deadline notices, and the kernel runs another round. The next answer
without a tool call ends the run, and `hya exec --bare` prints that answer by
default. An answer cut off at the output limit (`length`) also gets the check: the
extra round lets the model finish what the limit cut, and the check is still
sent only once, so the run ends on the next answer either way.
The check asks the model to compare the current result directly with the
request's wording. For a value the request says is unknown, the model tries
a value other than the one it can see.

```text
[hya] Completion check, sent once before this run ends. Compare the result with the request itself, not with your memory of it.
1. Re-read the request. List what it states literally: paths and file names, exact strings and output formats, commands and flags, values and counts, and anything it says is unknown or must not be assumed.
2. For each item, look at the result as it is now (the file on disk, the output of the real command) and compare it with the request's wording. Where the wording is exact, print the actual text and read it against the request's text, character for character. A program or pattern you wrote to do the comparison does not count, and neither does a substring or a summary. Where the request says a value is unknown, try the result with a value other than the one you can see.
3. If something differs, fix it and check again. If everything already matches, change nothing.
Then give your final answer.
```

The check is not sent, and the run ends on the current answer without a tool
call, when:

- the kernel has not yet run a call of a known tool (a run that only answered,
  or only called tools that do not exist, has nothing to check);
- the session log already holds the check (a run gets it once, also after a
  rewrite or a replay of the log);
- the round ended with an error finish (for example a provider content
  filter), which ends the run as it did before;
- the run is cancelled (checked before the check is appended; a cancel that
  lands right after it is logged ends the run as cancelled, with the check
  never sent).

With `--time-budget`, the check is sent only while more than twice the Final
window remains (more than 216 seconds for a 900-second budget or 432 seconds
for a 1,800-second budget), giving the check a few rounds and keeping the
Final window free for the last step.

The first two are read from the session log's projection, so a replayed log
yields the same decision, and a rewrite cannot make the check be sent again.
The opening line identifies a logged check even when its instructions change.
Like the deadline stage notices, the check is a transient notice: the next
request carries it as its last (user) message, and once the assistant message
has a part after it (the model's response to the check), requests leave it
out. What the model does in response (its reasoning, text and tool calls)
stays in the history and carries the work. Nothing configures or disables
it. The full-mode headless lead applies its completion check between turns.
The kernel loop asks the harness at this
point through `Harness::before_end`, only after a `stop` or `length` finish
and while the run is not cancelled.

### Turns without an answer

A model can spend a whole turn thinking and stop there, for example when the
upstream caps one response's output tokens below what hya asks for. Such a
turn is not an answer. An empty turn is a round that finished `stop` or
`length` without a tool call, whose parts from the round's start contain no
text part with a character other than whitespace. Reasoning does not count
as an answer.

After an empty turn, bare mode neither ends the run nor sends the completion
check. It appends one logged system message, in the same form as the
completion check, and runs another round:

```text
[hya] Your last turn ended with thinking only: it gave no answer and made no tool call, so nothing was done in it. It ended after {n} output tokens. One turn's output is limited, so think in shorter turns. Continue from what you worked out: make the next tool call now, or give your final answer. Write results you will need later to a file or the todo list instead of holding them in thought.
```

`{n}` is the output-token count the round reported. When the round reported
no usage, the sentence `It ended after {n} output tokens.` is left out.

Completed empty-turn reasoning stays in history with its provider replay data
intact. Each route can replay it according to its usual policy. In both modes
the next request also carries the notice as its trailing user message, with
the same bounded head-and-tail excerpt as a deadline cut. This gives endpoints
that ignore reasoning before a user message the context for the next step.

At most two empty turns in a row get this message. A third empty turn in a
row ends the run as any answer without a tool call does, and `hya exec
--bare` prints an empty answer. A round with a tool call or with answer text
resets the count. The message is sent in every stage of `--time-budget`,
also when the completion check no longer is, and never once the run is
cancelled. Full mode sends the notice only before the Final stage. A full-mode
`stop` requires non-whitespace reasoning or a positive reported reasoning-token
count. An empty reasoning block alone leaves the worker idle. A `length` finish
remains eligible without reasoning.

The completion check keeps its rules. An empty turn neither triggers it nor
counts as sending it: when a later round answers with text, the check is sent
or not exactly as described above. The message is transient: the next request carries it as its last
(user) message, and requests leave it out once the model has responded.

For example, a round thinks for six minutes and stops after 65,536 output
tokens without text. The log records the round as `StepFinished` with
`finish: stop`, then the system message containing `It ended after 65536
output tokens.`, then the next `StepStarted`. If that round calls a tool, the
run goes on as usual; if it answers with text after a tool already ran, the
completion check follows.

## When a bare round fails

Bare mode can recover from transient provider errors and interrupted streams.
When you run `hya exec --bare`, the kernel retries the same request up
to eight times per round, waiting 5, 10, 20, 40, 60, 60, 60, and 60 seconds
before the respective retries. A model route can need minutes to stop sending
requests to an upstream that throttles or has exhausted its allowance. Without
`--time-budget`, a persistently failing endpoint ends the run after about five
minutes of waiting, up from 35 seconds.

A retry requires all of the following.

- The provider reports a transient error or an established stream stalls or
  breaks while reading the HTTP body. Transient errors include transport
  errors before the stream and HTTP 429 or 5xx, including in-stream error
  frames classified as 429 or 5xx. An error opening the stream, after the
  provider's own attempts, follows the same retry rule. Other in-stream error
  frames are not retried.
- The run is active throughout the wait. With `--time-budget`, the full wait
  must fit before Final, and the budget must still be in Normal or WrapUp
  when the wait ends.

A round whose stream already delivered its final frame is complete. If the
stream then stalls or breaks while its body is read, the round counts as
collected with its finish, tool calls, and usage: it is neither retried nor
an error. Every provider protocol's final frame (`[DONE]`, `message_stop`, a
`finishReason`, `response.completed` or `response.incomplete`) is its last
meaningful one. An in-stream error frame after the final frame still fails
the round.

A retry does not depend on how far the failed attempt got. The attempt may
have persisted reasoning, text, tool input, or complete tool calls. None of
it was acted on: the kernel runs a round's tool calls, including `rewrite`
and `todo`, only after the round's stream ended without error. Once a retry
is selected, before the wait, the kernel appends one `part_deleted` event
for each part the attempt persisted. The attempt's own events stay in the
log as they were recorded, but its parts leave the assistant message. They
are not sent in the retry or any later request, and are never run. They are
not part of the answer `hya exec` prints, and they do not count as a tool
call for the completion check. They are left out of `rewrite`, any fold,
and the context estimate's count of the request. Usage the attempt reported
stays recorded and billed; it can be the latest round's usage that the
context estimate compares with, until the retry reports its own. If the
wait then ends in a cancel or the Final stage, the run ends with the
attempt's error and the parts stay deleted. The retry sends the failed
attempt's request again, so both `StepStarted` events carry the same `view`.

When a retry is ruled out before the wait, or the eighth retry fails, the
run ends with the current attempt's error, and that attempt's parts stay in
the message. A completed round resets the retry count.

For example, the route cuts a stream 90 seconds into a round while the model
is reasoning, and the run has time left. The event log records the attempt's
truncated reasoning as `reasoning_replace` and `reasoning_end`, then
`StepFinished` with `finish: error`, then a `part_deleted` for that reasoning
part. Bare mode waits 5 seconds and sends the same request again under the
next step number. If opening the stream returns HTTP 429, the same wait and
request replay apply. The failed open emits neither `StepStarted` nor
`StepFinished`, so the next attempt uses the same step number.

## Differences from full mode

Bare mode loads no bundle. It has no permission checks, hooks, subagents,
Workflows, compaction ladder, output truncation, or path bounds. Its only
timeout is how long a `bash` call waits.
These capabilities belong to full mode and its bundles. The kernel retains
its six tools, minimal prompt, 80% harness rewrite, and event logging.

Full mode's system prompt starts with the kernel prompt, composed from the
tools each round offers ([System prompt](#system-prompt)).

In full mode the `hya/base-tools` tools `read`, `write`, `edit`, and `bash`
replace the kernel's four under the same names; the kernel's versions run
only in bare mode. Full mode never registers a kernel tool. The kernel still
offers and runs `rewrite` and `todo` in both modes, and no other tool may be
offered under either name: a resource view whose model-facing tool name is
`rewrite` or `todo` fails to compile. `hya-engine`'s
`base_tools_replace_the_kernel_tools_under_the_same_names` test pins this.
`--pure` only removes external project/user context from full mode; it does
not select the kernel alone.

## How full mode runs on the kernel

Full mode is the `hya-engine` crate (`SessionEngine` and everything behind
the server and CLI); the kernel and `CoreError` are all of `hya-core`.
Full-mode turns run the same kernel loop as `--bare`; there is no second
round loop. Each round reads the projection, lets the harness rewrite it,
builds the request with `kernel::request` from the resulting view,
streams one model round, and runs its tool calls in order. The full mode
plugs into that loop at fixed points:

| Point | Bare mode | Full mode |
| --- | --- | --- |
| Before the round (`Harness::before_round`) | `kernel::before_round` with no extensions: the 80% Rewrite, then the goal checkpoint | Re-validate the actor claim, rebind a Root turn to a new runtime generation, resolve the round's model, then `kernel::before_round` with the internal bundles' extensions (`hya/compaction`: [`shake` and `remote`](compaction.md)) |
| Request | Kernel prompt and six tools | The agent's system prompt, filtered tool schemas plus `rewrite` and `todo`, and reasoning effort, then the `chat.params` hook |
| Opening the stream | The given model | Cross-model fallback or the Workflow route, under a governor stream permit |
| Failed round | Retried up to eight times after 5, 10, 20, 40, 60, 60, 60, and 60 s on a transient error opening or reading the stream, or a stream interruption, when the wait fits before Final, also after partial output; the failed attempt's parts are deleted from the message before the wait ([When a bare round fails](#when-a-bare-round-fails)) | After the stream opens, retried up to twice on a link error before any text or tool call |
| Registry tool call (`ToolGate`) | Execute the tool and record file snapshots and stamps | Hooks, permissions, the full `ToolCtx`, file snapshots and stamps, output capping, steer notices |
| Unrun calls after a cancel | Failed as `cancelled` | Left open; the turn end closes them as `CANCELLED` |
| End (`Harness::before_end`, `done`) | The round's finish, after one [completion check](#how-a-bare-run-ends) once a tool call ran; a turn with neither answer text nor a tool call gets a [notice and another round](#turns-without-an-answer), at most twice in a row | The round's finish; also ends after an accepted `report` |

The kernel offers and handles the model's `rewrite` and `todo` calls in both
modes. `shake`'s `ContextEvicted` events store the exact notice for each
evicted part, which the projection renders during replay.
`TurnBindingRecorded` records the runtime generation and the pre-hook
system prompt and tool list digests at turn start and whenever those
digests change. `StepStarted.view` fingerprints the request content after
`chat.params` and before provider routing.

Full mode records its initial environment, project instructions, and
discovered skill index before the first model call. The turn harness reads
the resulting `initial_context` from the round's projection. A legacy session
records it at the first model turn run by the new runtime.
[Project context](configuration.md#project-context-agentsmd)

## Rewrite contract

The contract an extension is written against lives in `hya-tool`
(`hya_tool::rewrite`), because a native bundle links only `hya-tool` and
`hya-proto`. All of it is part of the native ABI digest.

| Item | Implemented by | Purpose |
| --- | --- | --- |
| `Rewriter { before_round(cx), rewrite_now(cx) }` | internal bundles | A context-reduction extension. `before_round` runs before every round and reduces only when `cx.pressure` is over; `rewrite_now` reduces on request (`/summarize`). `Ok(true)` when the log changed. Stateless (`&self`) |
| `RewriteCtx` | the host, per call | Session, assistant message, round model and its `ModelWindow` (window, usage reporting, reasoning replay, tool image support), projection, system-and-tools `overhead_tokens`, `pressure`, workdir, the extension's JSON `config`, and the services below |
| `Pressure { used, threshold }` | the kernel | The request's size against 80% of the window; `None` without a window. `over()` is `used > threshold` |
| `SessionLog { projection, append }` | bare: `kernel::Scope` over the store; full mode: the turn's actor-claimed emit | Read the projection, append one event batch |
| `Transcript { messages(projection) }` | bare: the kernel agent's rendering; full mode: the agent's rendering with the round's prompt images (`/summarize`: the current episode) | The messages a round sends for a projection |
| `ModelHost { compact_window(model, messages) }` | full mode only (`None` in bare) | Route-native `/responses/compact`; `None` when the route has none |
| `CompactionHooks { before(session, tokens), after(session, summary_tokens) }` | full mode only (`SessionEngine`) | The `compaction.before` (overflow) and `compaction.after` hook points; the host ignores the `compaction.before` reply |

The kernel drives the reduction. `kernel::before_round(cx, extensions)`
measures the pressure, calls each extension's `before_round` in order, and
then rewrites itself as described in [Rewrite rules](#rewrite-rules);
`kernel::rewrite_now(cx, extensions)` runs each extension's `rewrite_now`
until one changes the log, otherwise the kernel's own Rewrite. Bare mode
passes no extensions. Full mode passes every rewriter the internal bundles
register, in registration order (today `hya/compaction`,
[Compaction](compaction.md)); nothing replaces the kernel's Rewrite.
`SessionEngine::new` loads them once per process with
`hya_core::bundles::load_internal()` and panics when a listed bundle's library
cannot be loaded, as a missing tool family does. Full mode's `config` is the
engine's `keep_recent` and token-accounting mode, as JSON. A host failure
reaches the kernel or an extension as a `ToolError`; the host keeps the
original `CoreError` and a failed reduction ends the turn with it (for
example a stale actor claim).
Token estimation (`hya_tool::tokens`) is shared by host and bundles, and the
reasoning replay policy and compacted-context markers it and the request
encoders read are in `hya_proto::context`.
