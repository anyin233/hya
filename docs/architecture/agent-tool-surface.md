# Agent Tool Surface

This document describes the tools that hya exposes to agents, with detailed
coverage of file access, local search, interaction, network, and mailbox tools.
It distinguishes three related but different surfaces:

1. **Registered**: a name resolves in `ToolRegistry`.
2. **Advertised**: a canonical schema is included in a model request or the
   v1 tool catalog.
3. **Executable**: the tool has the runtime plane, session, permissions, and
   other resources needed to complete a call.

The distinction matters because aliases resolve but are not advertised,
model filters hide some registered schemas, and several always-
registered builtins delegate to runtime planes that can be disconnected or
empty. The registry stores canonical tools and aliases separately, and only
canonical tools contribute schemas.
([`ToolRegistryInner`](../../crates/hya-tool/src/tool.rs),
[`ToolRegistry::schemas`](../../crates/hya-tool/src/tool.rs),
[`ToolRegistrySnapshot::schemas`](../../crates/hya-tool/src/tool.rs))

## Builtin inventory

`ToolRegistry::builtins()` installs **27** canonical schema names before model
filtering. The inventory below is complete for that constructor. A schema marked
**closed** rejects unknown keys; aliases resolve only at runtime and are never
advertised.
([crates/hya-tool/src/tool.rs](../../crates/hya-tool/src/tool.rs))

| Area | Canonical schema names | Role |
| --- | --- | --- |
| File access | `read`, `write`, `edit`, `apply_patch` | Read or mutate workspace files. `read`, `write`, and `edit` are native coding tools; `apply_patch` remains the separate patch envelope. |
| Local discovery | `ls`, `glob`, `find`, `grep`, `lsp` | List directories, match paths, search text, or query language servers. |
| Commands | `bash` | Run a command with bounded capture; hidden runtime name `shell` is not advertised. |
| Human/session interaction | `ask_user`, `todo__read`, `todo__update_status`, `todo__update_content`, `plan_exit`, `invalid` | Ask batched structured questions, read/update session todos, request a plan-mode transition, or represent invalid tool arguments. |
| Agents and teams | `skill`, `list_agents`, `task`, `workflow`, `search_agent`, `archive`, `wait` | Load skills, discover/spawn agents, execute governed Workflow commands, search archived subagents, stop-and-archive a subagent, and block until subagents finish (`wait` stays advertised at every depth). The orchestration plane is hidden at depth 2 ([ADR-0015](../adr/0015-unified-resident-subagent-lifecycle.md)). |
| Communication | `send`, `list_channel`, `report` | Channel-plane communication: one channel-addressed send (the channel's nature picks DM vs broadcast, with archive revival), channel listing, and terminal reports ([ADR-0016](../adr/0016-channel-communication-plane.md)). |
| Network | `webfetch`, `websearch` | Fetch a URL or run provider-backed web search. |

### ask_user

`ask_user` is the single canonical question tool: a batch of structured
questions under a top-level `questions` array, routed through the
InteractionPlane. Each item requires `question`, `header`, and `options`
(each option is `{label, description}`; an empty list makes the question
free text). Optional fields are `multiple` (allow several selections),
`allow_custom` (allow a write-in answer outside the option list; defaults
to true when omitted; legacy `question` calls may spell it `custom`), and
`default` (default free-text answer).

The result carries structured per-question entries in
`metadata.answers` — `{question, answer: [chosen values], cancelled}` —
plus a human-readable `output` line where an unanswered question renders
as `Unanswered`. Cancellation is reported per question, not as an error.
Plane failures (no host attached) surface as a tool error instead of a
silent empty answer.
([crates/hya-tool/src/ask_user.rs](../../crates/hya-tool/src/ask_user.rs))

### The `todo__` namespace

Session todos live behind three namespaced tools sharing one plane
([crates/hya-tool/src/todo.rs](../../crates/hya-tool/src/todo.rs)):

- `todo__read` — the current list; items are `{id, content, status}` with
  stable plane-assigned ids ("1", "2", …) that are never reused.
- `todo__update_status` — batch `{id, status}` updates; statuses are
  `pending`, `in_progress`, `blocked`, `completed`.
- `todo__update_content` — batch `add` / `remove` / `edit` operations,
  validated as a whole before anything is applied (a bad id fails the
  batch with the current ids listed, leaving the list untouched).

Every write returns the full snapshot in `metadata.todos`, which is what
the engine's replay fold consumes; the pre-0.36.53 `todowrite` spelling
is removed from dispatch, but historical sessions still replay (their
rows get synthesized `todo-{index}` ids and lenient status mapping).
The wire enum gained `TODO_STATUS_BLOCKED`.

### Task

`task` launches one subagent or a multi-member batch. Under the unified
lifecycle ([ADR-0015](../adr/0015-unified-resident-subagent-lifecycle.md))
every spawn is a **non-blocking resident**: the call returns immediately with
the agent's handle and session; results arrive later as the agent's `report`
mail. Nested `task` calls are allowed up to the hardcoded two-layer depth cap
(`MAX_SUBAGENT_DEPTH = 2`); total fan-out is bounded by the
`SubagentGovernor` per-run budget. Execution requires a session and checks
`Action::Task` for every member; the spawner also enforces the caller's
`can_spawn` roster (unknown or disallowed agents surface as
`unknown_agent_id` / `agent_spawn_not_allowed`). An empty or omitted
`subagent_type` normalizes to `"general"`; a non-empty unknown id does not
fall back to `general`.

| Parameter | Role |
| --- | --- |
| `description` | Short label (required with single-member form). |
| `prompt` | Work for the agent (required). |
| `subagent_type` | Agent id — the only way to choose the agent (also per member); empty/omitted normalizes to `"general"`. It also names the member: the handle leaf is `<subagent_type>-<operator>`, where the resolved agent id is sanitized (lowercased; other characters → `-`; at most 32 characters) and the harness appends one random operator name (`"subagent_type": "scout"` → `main/scout-suzuran`; omitted → `main/general-amiya`). |
| `category` | Logical model-category override. |
| `model` | Concrete provider/model override (wins over category). |
| `command` | Optional command that triggered the task. |
| `inline_agent` | Request-scoped overlay. Published fields are `name`, `prompt`, `category`, and `model`; nested `description` is not advertised. |
| `members[]` | Fan one call out to several subagents (each needs `prompt`; optional per-member overrides). |

Handles are never reused within a team (live or archived members), so a bare
leaf always names one member and mail to an archived member's handle wakes
that member. The naming rules, the name list's provenance, and replay
behavior are specified in
[subagent-orchestration.md §2.1](subagent-orchestration.md#21-handles-agent-type--operator-name-0410).
Example:

```json
{"description": "map the parser", "prompt": "find every entry point", "subagent_type": "scout"}
```

returns `Resident main/scout-suzuran is live; results arrive as its report.`

**Removed `name` (0.41.0, breaking).** `task` no longer takes a `name` (top
level or per member). A call that still passes one fails with an `input`
error — "`name` was removed; the handle is derived from `subagent_type`. …" —
and spawns nothing, because a caller that meant `name` to pick the agent would
otherwise silently get `general`.

Removed fields: `task_id` (resume is superseded by mail revival of archived
agents), `background` (every spawn is non-blocking), and `resident` (every
agent is resident). Stale callers sending them are ignored by the closed-parse
schema.

The nested `inline_agent.description` parser field is retained only to handle
stale/direct callers. Empty or whitespace-only values normalize to absence, so
the captured empty-description request can spawn. A non-empty value is rejected
before admission with typed wire error `unsupported_inline_agent_field`, with no
child or session side effect. This hidden compatibility does not change
authorization, model/category precedence, resident behavior, or run-tree
projection.

([crates/hya-tool/src/task.rs](../../crates/hya-tool/src/task.rs))

### Skill

The only parameter is `name`. It must be a skill name from the
`available_skills` list injected into the system prompt (not a path). The tool
asserts `Action::Skill` on `Resource::Skill(name)`, then returns a
`<skill_content>` envelope containing the SKILL.md body, the skill's absolute
base directory as a `file://` URL (relative paths inside the skill resolve
against it), and a sampled `<skill_files>` listing capped at
`FILE_SAMPLE_LIMIT = 10` entries (recursive, sorted, excluding `SKILL.md`
itself). The output states that the file list is sampled, so a skill with more
than 10 supporting files shows an incomplete list. See also
[`docs/skills.md`](../skills.md).
([crates/hya-tool/src/skill.rs:14](../../crates/hya-tool/src/skill.rs#L14),
[crates/hya-tool/src/skill.rs:95-150](../../crates/hya-tool/src/skill.rs#L95-L150))

### Todowrite

Input is a `todos` array of `{content, status, priority}` objects. The call
**replaces** the session's whole todo list rather than appending. The list lives
in the in-memory `TodoPlane` (not persisted independently of the event log). The
result echoes the list back with a title carrying the count of still-open items
(status other than `"completed"`). Alias: `todo`.
([crates/hya-tool/src/todo.rs:75-136](../../crates/hya-tool/src/todo.rs#L75-L136))

### Coordination tools (allocated at startup)

An agent's **coordination tools** are allocated by the harness when the agent
starts, the same way for built-in agents and for agents imported from bundles,
for roots and for members, with or without a `resource_view`. A bundle
`resource_view` narrows only *domain* tools (`read`, `write`, `edit`, `bash`,
`grep`, `glob`, MCP, skills, …); it cannot take away the tools an agent needs to
take part in a team. Before 0.41.0 a bundle subagent whose `allow` list named
only `read`/`grep`/`glob`/`report` (the `hya-extra/scout` bundle) could never
finish once its lead mailed it: every `report` was rejected for unread mail and
it had no `list_channel` to find the channel holding it.

There is nothing to configure. A manifest lists only its domain tools:

```yaml
agents:
  - id: scout
    role: subagent
    resource_view:
      allow: [harness:tool/read, harness:tool/grep, harness:tool/glob, zvec-grep]
```

and the harness adds `report`, `wait`, `send`, `list_channel`, and channel
reads. Listing a coordination tool explicitly (`harness:tool/report`) still
works; it is deduplicated with the injected set.

The allocation is decided when the agent's view is compiled (per turn, from
the bound catalog) and filtered by depth when each request is built:

| Tool | Allocated to | Advertised |
| --- | --- | --- |
| `report` | every agent | subagents only (depth ≥ 1); the root never sees it |
| `wait` | every agent | every depth; the mail-aware channel-tools `wait` when that family is loaded |
| `task`, `archive` | agents with **spawn rights**: a built-in (ordinary spawn scope) or a bundle agent whose `can_spawn` names an installed agent | depth 0 and 1; hidden at the depth cap (`MAX_SUBAGENT_DEPTH` = 2) |
| `send`, `list_channel` | every agent, when the channel family (`hya/channel-tools`) is loaded | every depth |
| `read channel://<id>` | every agent, when the channel family is loaded | every depth; a view that does not select `read` gets a **mail-only** `read` that serves `channel://` handles (and `#<id>` / `#<member handle>`) and refuses every other path |

Resulting sets (channel family loaded — the default builtin registry):

| Agent kind | Depth | Coordination tools |
| --- | --- | --- |
| Root (built-in, or bundle `role: main`) with spawn rights | 0 | `wait`, `task`, `archive`, `send`, `list_channel`, `read channel://` |
| Root without spawn rights | 0 | `wait`, `send`, `list_channel`, `read channel://` |
| Subagent with spawn rights | 1 | `report`, `wait`, `task`, `archive`, `send`, `list_channel`, `read channel://` |
| Subagent without spawn rights (e.g. `scout`) | ≥ 1 | `report`, `wait`, `send`, `list_channel`, `read channel://` |
| Any subagent at the depth cap | 2 | `report`, `wait`, `send`, `list_channel`, `read channel://` |

Without the channel family there are no channel tools and no `report` (it
belongs to that family): every agent gets the member-only `wait`, plus `task`
and `archive` with spawn rights below the cap. An agent that can spawn nobody
never sees `task` or `archive`, even with a default (unnarrowed) view.

`deny` semantics: `resource_view.deny` may remove any coordination tool except
`report` — denying `task`, `archive`, `wait`, `send`, or `list_channel` is
honored. Denying `harness:tool/report` (in any spelling) rejects the view with
`InvalidManifest` (``resource_view.deny `…` removes the coordination tool
`report`; a subagent without it can never finish``). Denying
`harness:tool/read` removes file reading only: the mail-only `read` is still
allocated, so the report gate can always be satisfied. When the view already
gives a coordination tool's bare name to one of its own resources (a
bundle-local tool, an MCP server, or a `resource_view.aliases` key), the
bundle's resource keeps the name and the harness tool is not injected, so no
`NamespaceCollision` is introduced.

The team quick reference appended to every request's system prompt is
rendered from the tools that request advertises
([`hya_core::prompt::team_quick_reference`](../../crates/hya-core/src/prompt.rs)):
each line appears only when the agent has the tools it teaches, and the root
gets the "never call `report`" line instead of the subagent's "finish with
`report`" line. Implementation:
[`crates/hya-core/src/coordination.rs`](../../crates/hya-core/src/coordination.rs)
and `select_candidates_globally` in
[`runtime_registry.rs`](../../crates/hya-core/src/runtime_registry.rs).

### Communication tools (ADR-0016 channel plane)

All communication tools report that they are available only inside a running
team when the mailbox plane is disconnected. The per-request team quick
reference mirrors this contract and is guarded by a registry-alignment test
(every tool-named token it teaches must resolve in the builtin registry).

**`send`**: required `body`; optional `channel` (the legacy `to` spelling
still parses). One tool, channel-decided delivery:

- `#channel` (or a bare `DM-…`/`announce-…` id from `list_channel`) posts on
  that channel. Group channels are the unit leader's broadcast pipe — a
  one-way announcement every live member hears; posting is leader-only. DM
  channels stay private 1:1 chatter.
- A bare handle sends private mail to that vertical peer. Subordinates name
  their one upward peer with `^parent`; mail to an archived direct child
  **revives** it with its saved handoff state (ADR-0015). Siblings are not
  addressable; out-of-scope targets are indistinguishable from unknown.
- Omitted, `send` uses the sender's default channel: the unit group pipe
  when the sender leads one, else the parent DM pair.

Group channels never expose a member list. Archived members are no longer
members: a group post never reaches them.

**`list_channel`**: no parameters; lists the caller's channels — group pipes
with a can-post flag and DM channels with peer identity and unread counts.
Archived peers' DM channels are excluded (use `search_agent`).

**`report`** (channel-tools family): required `result`, optional `outcome`
(`done`/`failed`). Rejected while the caller has unread mail or live direct
subagents. Mail the caller's current resident wake already put into its turn
counts as read. The unread-mail rejection names every channel holding unread
mail and the exact read call, for example:

```text
report rejected: `main/scout-suzuran` has 2 unread mail message(s) on #DM-rgli51cb (2); answer them first. Read it with `read channel://DM-rgli51cb?last=2` (`list_channel` lists every channel with unread counts), reply with `send` if the sender needs an answer, then call `report` again — or call `wait` to block until more mail arrives.
```

Handle-addressed mail is attributed to the DM channel shared with its sender;
harness notices with no channel are counted as `N harness notice(s)` and
arrive with the next tool result as `[NEW MAIL]`. Reading any channel marks
the whole inbox seen.

**`read channel://<id>`** accepts a channel id from `list_channel` (`DM-…`,
`announce-…`, `<unit>#<name>`; leading `#`s and padding are stripped with a
warning). It also accepts a **member handle**, canonical (`main/scout-suzuran`)
or a leaf (`scout-suzuran`). A handle reads the caller's DM with that member,
and the output starts with a warning that names the DM id:

```text
[warning] normalized channel id `#main/scout-suzuran` → `main/scout-suzuran`; `#main/scout-suzuran` is a member handle, not a channel: showing your DM with it, `DM-rgli51cb` (read it as `read channel://DM-rgli51cb`)
```

A real channel id always wins. Channel ids are minted `DM-…`/`announce-…`
keys or `#`-qualified unit keys, never a bare member path, so a handle can
never shadow a channel. A bare `read #<handle>` path that names no existing
file is served the same way; a file whose name starts with `#` (an editor's
`#draft#`) still reads as a file. When no such channel or DM exists, the error
lists the caller's own channels (DM peers named) and points to `list_channel`:

```text
unknown channel `#main/nobody`; your channels: `DM-rgli51cb` (DM with main/scout-suzuran). Read one with `read channel://<id>`; `list_channel` lists them with unread counts.
```

An accepted report returns `{"title": "Report accepted", "output": "Report
accepted; your turn ends now. You will be archived with a state handoff; mail
from your parent can wake you later."}` and **ends the caller's turn** after
the current tool round: the round's other tool calls complete and are
recorded, then no further model call is made (`MessageFinished` with
`finish: stop`). A second `report` in the same turn fails with:

```text
your report was already accepted in this episode and your turn ends after this tool round; do not call `report` again. If your parent mails you later you are woken for a new episode and may report once more.
```

See [subagent-orchestration.md §3.1](subagent-orchestration.md#an-accepted-report-ends-the-turn).

**`search_agent`**: optional `query` (free text over the goal/pending digests
of archived agents' final handoffs); lists the caller's own archived direct
children with handle, agent type, digests, and a degraded flag. `send` to
the returned handle to revive.

**`archive`** (extended-tools family, permission `task`; replaces `kill`,
removed in 0.41.0): required `target`, optional `reason`.

```json
{"target": "main/hya-worker-exusiai", "reason": "superseded by the new plan"}
```

`target` is the subagent's canonical handle, a leaf relative to the caller
(`hya-worker-exusiai`), or its session id (`hysec_…`) — the `task` result's
`member`/`session`. The target must be a live descendant of the caller. Its
own live subagents are archived first (deepest first). For each member an
in-flight turn is cancelled (`MessageFinished { finish: cancelled, cause:
archived }`, open tool parts `CANCELLED`) and waited for up to 5 s, then the
member is archived: degraded handoff, member row `cancelled` with the reason,
claim released, `AgentArchived { reason: archived_by_parent }`. No report mail
comes back. The result is `{title, output, metadata: {handle, session,
cancelled_turn, descendants}}`. An archived subagent stays readable (its
session log, channel history, handoff) and is woken by mail to its handle or
its DM channel — same handle, same session, resumed from the handoff.

Errors (`ToolError::Input`, actionable text):

| Case | Message |
| --- | --- |
| unknown target | ``no subagent `X` on your team; your live subagents: …`` |
| already archived | ``` `H` is already archived; send it mail (`send` to `H`) to wake it``` |
| the lead | ``` `main` is the team lead; the lead is never archived``` |
| not a descendant | ``` `H` is not one of your subagents; only its parent `P` or an ancestor can archive it``` |

**`wait`** (permission `read_only`; advertised at every depth): block the
calling turn until subagents finish, bounded by a timeout.

```json
{"targets": ["main/hya-worker-exusiai", "main/hya-worker-texas"], "mode": "any", "timeout_secs": 300}
```

| Field | Type | Contract |
| --- | --- | --- |
| `targets` | string[] (optional) | Handles as returned by `task` (canonical, or a leaf relative to the caller) or session ids; every one must be a descendant of the caller. Omitted: all live **direct** subagents. The whole value `"any"`/`"all"` is accepted as the mode over all live subagents. |
| `mode` | `"all"` (default) \| `"any"` | Return when every target / the first target finished during this call. |
| `timeout_secs` | integer, default 600, clamped to 1800 | `0` returns the current state at once without blocking. |

A target **finishes** only when it reports (`report`, then its archive
commits) or is archived without a report (`archive` by an ancestor, a drain,
teardown). Going idle is never finishing: a member between turns with work
owed (a running turn, queued mail or directive, an accepted report still
executing, or live subagents of its own) is `working`.

Every call is measured against a baseline taken when it starts, so a repeated
call never returns the same news twice:

| Situation at the start / during the call | Result |
| --- | --- |
| Target reports or is archived during the call | listed in `finished` with its report; counts toward `any`/`all` → `woke_by: members` |
| Target had already reported or been archived before the call (not woken since) | listed in `already_finished` with its report; never counts toward `any`/`all` and never wakes the wait |
| Every target already finished (or no live subagent) | returns at once with `woke_by: nothing_to_wait_for`; calling again gives the same answer, never `members` |
| Target woken again after its report (mail to an archived member revives it) | `working` until its **next** report; the earlier report is not repeated (the member's terminal handoff generation, bumped by every archive, tells the finishes apart) |
| Target's turn ended without a report and nothing is queued for it | `idle` under `running`; it will not continue until it is mailed, so the wait returns `woke_by: stalled` — once per stopped turn; a repeated wait on the same stopped member blocks |
| New mail for the caller (channel-tools `wait` only) | `woke_by: mail`; returned once (see below) |
| Nothing new before the deadline | `woke_by: timeout` (`timeout_secs: 0`: the current state at once) |

When several apply at one evaluation the order is `members`, `mail`,
`stalled`, `nothing_to_wait_for`, `timeout`.

**Result shape and budget.** The result is `{title, output, metadata}`, and
the wait budgets it itself: the whole envelope, serialized, stays within
`WAIT_RESULT_BUDGET` = 4500 characters, under the generic 5000-character tool
output cap, so the cap never cuts it and `metadata` always arrives intact.
`output` always starts with a compact header, then the previews:

```text
Subagents finished.
- main/hya-implementer-exusiai [reported] done
- main/scout-suzuran [already reported] done
Still running: main/hya-reviewer-texas
1 new mail message(s), now marked read (history: read channel://<id>?last=N).

Report from main/hya-implementer-exusiai (done):
Implemented the parser …
[… 2693 more chars; full text: read channel://DM-rgli51cb]

Report from main/scout-suzuran (done):
Entry points: src/cli.rs:12, src/lib.rs:40

Mail from main/hya-reviewer-texas @DM-k2m9x0qa:
Found two issues …
```

The header has the reason it woke, one line per finished/already-finished
target (handle, state, outcome), idle targets, the still-running handles, and
the new-mail count. Every report (or archive note) and mail body then gets a
preview. Short bodies are shown whole, and the budget left over is shared
equally by the longer ones. A cut preview ends with the omitted size and,
when the caller can read the full text, `read channel://<DM id>`. That is the
parent's DM with the member, where the report mail lives, or the channel the
mail came on. No pointer is printed when there is no such channel (for
example, a grandchild's report mailed to its own parent).

`metadata` carries the structure without the bodies:

```json
{"woke_by": "members", "waited_ms": 5210,
 "finished": [{"handle": "main/hya-implementer-exusiai", "session": "hysec_…", "state": "reported", "outcome": "done", "channel": "DM-rgli51cb", "report_chars": 3028, "report_truncated": true}],
 "already_finished": [{"handle": "main/scout-suzuran", "session": "hysec_…", "state": "reported", "outcome": "done", "channel": "DM-x81ka0ld", "report_chars": 44, "report_truncated": false}],
 "running":  [{"handle": "main/hya-reviewer-texas", "session": "hysec_…", "state": "working"}],
 "mail": [{"from": "main/hya-reviewer-texas", "channel": "DM-k2m9x0qa", "chars": 212}]}
```

`woke_by` is `members`, `mail`, `stalled`, `timeout`, or `nothing_to_wait_for`
(no target left: the caller has no live subagents or every target already
finished; a subagent with the mail-aware wait and no subagents instead waits
for mail). `state` is `reported` or `archived` (in `finished` /
`already_finished`) and `working` or `idle` (in `running`); `outcome` is
`done`/`failed` for a report and `cancelled` for an archive. `report_chars`
and `report_truncated` describe the report or archive note (the output shows
its preview) and are omitted when there is none — a working or idle member's
in-progress text is never surfaced as a report. `channel` is the DM holding
the report, present only when the caller is the target's parent. A mail
entry's `chars` is the length of its body as received (at most 600, the
`[NEW MAIL]` bound).
`already_finished` and `mail` are omitted when empty. The wait runs inside the
caller's turn and is woken through the engine bus (team-lifecycle events on the
root or caller log), never by a resident wake of the caller — that would queue
behind the waiting turn. Cancelling the turn (user cancel, drain) aborts it at
once (`ToolError::Cancelled`). Unknown or foreign targets are input errors that
list the caller's live subagents. Stall notices are remembered per
(caller, member, last assistant message) in the resident supervisor's memory;
after a process restart a still-stopped member is reported once more.

Two implementations exist: the `hya/extended-tools` `wait` wakes on member
finishes and stalls only; the `hya/channel-tools` `wait` **overrides** it
whenever the channel family is loaded (an explicit `overrides:
hya/extended-tools` in its exposure policy, see
[Tool-family presets](../base-tools.md#overrides)) and also returns when new
mail reaches the caller — from a subagent, the parent, or the harness
(`LEADER FAILED` wrap-up notices included) — with `woke_by: mail` and
each message previewed in `output` (the body is bounded to 600 chars like the
`[NEW MAIL]` notice, then to the shared budget; history stays readable with
`read channel://<id>`) and listed in `metadata.mail` as `{from, channel?,
chars}`. Mail is **new** only past the caller's durable inbox
cursor — the same `MailConsumed` cursor in-turn steering and resident wakes
use — and a wait that returns commits the cursor through the mail it
accounted for: the channel-aware wait through the whole inbox, the
member-only wait through the leading run of report mails of targets it
reported `finished`. So a returned message is never returned by a later
`wait`, never repeated in a `[NEW MAIL]` notice, and never re-injected by a
later resident wake of the caller. A finished target's report mail is its
finish, not a mail wake (it is not listed under `mail`); a report mailed while
the target's archive is still committing holds the wait for that archive.

Removed tools: `roster`, `channels`, `join`, `leave` — their information folds
into `list_channel`/`search_agent`; named user-created channels no longer
exist. `kill` became `archive` (0.41.0).

`list_agents` enumerates definitions usable by `task`.
([crates/hya-tool/src/agents.rs:22-84](../../crates/hya-tool/src/agents.rs#L22-L84))

### Bash

`bash` is the sole model-facing command tool. Its closed base schema is:

```json
{
  "command": "string",
  "env": { "string": "string" },
  "timeout": "number (seconds)",
  "cwd": "string",
  "pty": "boolean"
}
```

Only `command` is required. The default timeout is 300 seconds; `timeout: 0`
disables the deadline, and other finite values clamp to 1..=3600 seconds with a
clamp notice. `cwd` is checked against the existing lexical workdir policy.
Command permission is checked before process creation. Timeout and cancellation
terminate and reap the complete process group. Non-PTY stdout/stderr are
captured concurrently in arrival order; PTY mode uses a real PTY and keeps
observing the deadline/cancellation after leader exit while descendants retain
the slave. Inline output is capped at 50 KiB after timeout/clamp notices are
added. A truncated result points to the complete raw stream in a private
mode-0600 hya artifact; an armed owner removes partial/unpublished artifacts on
every other exit. Nonzero exits and timeouts are completed structured results
with status metadata, while explicit cancellation is typed `cancelled`.
Environment values are never echoed in titles, output, diagnostics, metadata,
or any client surface.

The old `shell` name remains only as a hidden runtime alias for stale callers;
it is not an advertised schema and uses the same implementation. This is
intentional compatibility, not a second command surface.
([crates/hya-tool/src/shell.rs](../../crates/hya-tool/src/shell.rs),
[crates/hya-tool/src/tool.rs](../../crates/hya-tool/src/tool.rs))

### Webfetch

Parameters: `url` (http/https only), `format` = `text` | `markdown` (default) |
`html`, and `timeout` in seconds — default 30 s, clamped to a maximum of 120 s.
Responses larger than 5 MB are rejected. Responses whose content type is
jpeg/png/gif/webp are returned as base64 data-URI attachments instead of text.
The tool asserts `Action::WebFetch` on `Resource::Url(url)` and carries
`ToolPermission::Tool`, so under `permission.model: default` it asks before every
fetch.
([crates/hya-tool/src/webfetch/mod.rs:18-27](../../crates/hya-tool/src/webfetch/mod.rs#L18-L27))

### Websearch

Call parameters (in addition to the provider discussion below):

| Parameter | Default / values |
| --- | --- |
| `query` | Required. |
| `numResults` | Default **8** (Exa path). |
| `livecrawl` | `fallback` (default) \| `preferred`. |
| `type` | `auto` (default) \| `fast` \| `deep`. |
| `contextMaxCharacters` | Schema text advertises 10000; the Exa client forwards the field only when the caller supplies it. |

The tool is itself an MCP client: Exa is called at `https://mcp.exa.ai/mcp` with
the key appended as an `?exaApiKey` query parameter; Parallel at
`https://search.parallel.ai/mcp` with a bearer token. It asserts
`Action::WebSearch` on `Resource::WebSearch(query)`.
([crates/hya-tool/src/websearch.rs:16-18](../../crates/hya-tool/src/websearch.rs#L16-L18),
[crates/hya-tool/src/websearch.rs:132-171](../../crates/hya-tool/src/websearch.rs#L132-L171))

### Hidden aliases

Six legacy aliases resolve during execution but do not appear in
`ToolRegistry::schemas()`:

| Canonical advertised name | Hidden lookup alias |
| --- | --- |
| `bash` | `shell` |
| `webfetch` | `fetch` |
| `websearch` | `search` |
| `apply_patch` | `patch` |
| `plan_exit` | `plan` |
| `ask_user` | `question` |

The `shell` entry is the only compatibility spelling for the command tool; it
uses the canonical Bash schema and permission path. The other aliases are
existing registry conveniences. All aliases remain non-advertised and never
change a canonical schema's input fields.
([`crates/hya-tool/src/tool.rs`](../../crates/hya-tool/src/tool.rs))

## Advertisement and naming

Before each completion request, hya obtains canonical registry schemas and
applies an advertisement-only filter:

- Hashline `write` and `edit` are advertised to **every** model.
- `apply_patch` is never advertised. It remains registered for hidden `patch`
  dispatch.
- enabled `websearch` is advertised to every model provider.
- `report` is never advertised to the root (depth 0); `task`, `list_agents`,
  `workflow`, `search_agent`, and `archive` are not advertised at the depth
  cap.
- Every other canonical schema passes through.

The schemas start from the agent's compiled view, which already carries the
harness-allocated [coordination tools](#coordination-tools-allocated-at-startup).

The tools remain registered even when their schemas are filtered from the
request. File mutation on the model-facing path is the hashline `write`/`edit`
pair, not the Codex-style `apply_patch` envelope.
([`hya_core::advertise_tool`](../../crates/hya-core/src/engine/turn/messages.rs))

### Why WEBSEARCH was provider-filtered

The removed `compat` restriction was inherited product policy, not a
model-protocol or tool-execution requirement.

The upstream OpenCode history is explicit. Commit
[`9c237f0`](https://github.com/anomalyco/opencode/commit/9c237f0bfb9335c8ce6c793c4eee0e17ef4d775e)
"temporarily restrict[ed] codesearch and websearch to opencode zen users" while
an enterprise opt-out was unresolved. Commit
[`419983c`](https://github.com/anomalyco/opencode/commit/419983c0f1dcffc4fae28f844e7658326e2ee5aa)
then restored an opt-in for non-Zen users through `OPENCODE_ENABLE_EXA`; its
[pull request](https://github.com/anomalyco/opencode/pull/5132) describes this as
an interim rollout rule. Current OpenCode keeps the same shape: web search is
enabled for its `opencode` provider or when explicit Exa/Parallel flags are set.
([current registry](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/tool/registry.ts),
[Parallel rollout](https://github.com/anomalyco/opencode/pull/26227))

hya commit `fd96760794056ce9eacaad9c6d72768863d890c6` copied the strict
`provider == "opencode"` branch. Commit
`07af114e9284ad3a79c62fa777cecb96a766e91f` later changed that provider string
to `compat` as part of a broad external-compat debranding change. It did not add
the upstream opt-in flags or establish `compat` as a web-search capability.

That distinction matters because hya's provider IDs are user-defined config
keys, and Compat config import preserves those IDs. A provider named `compat`
is therefore neither required nor sufficient to identify OpenCode Zen.
([crates/hya-app/src/config.rs](../../crates/hya-app/src/config.rs),
[crates/hya-app/src/config.rs](../../crates/hya-app/src/config.rs))

The execution path itself never examines the model provider. `tools.websearch`
selects Exa or Parallel, optionally overrides the endpoint and key, and can
disable the built-in. Exa is the enabled, unauthenticated default. Exa keys are
sent as `exaApiKey` query parameters; Parallel keys are sent as bearer tokens.
([crates/hya-tool/src/websearch.rs:22-72](../../crates/hya-tool/src/websearch.rs#L22-L72),
[crates/hya-tool/src/websearch.rs:157-171](../../crates/hya-tool/src/websearch.rs#L157-L171))

The stale compatibility condition was removed. Enabled websearch is now
advertised independently of the model provider.

The OpenAI, Anthropic, and Google request encoders preserve the canonical
`ToolSchema.name`; they translate only the surrounding provider JSON shape.
Descriptions and input schemas are forwarded with the same values.
([crates/hya-provider/src/openai.rs:23-75](../../crates/hya-provider/src/openai.rs#L23-L75),
[crates/hya-provider/src/anthropic.rs:15-57](../../crates/hya-provider/src/anthropic.rs#L15-L57),
[crates/hya-provider/src/google.rs:139-194](../../crates/hya-provider/src/google.rs#L139-L194))

The v1 tool catalog (`GET /v1/tools` and the bootstrap snapshot) surfaces the
configured registry without a websearch model/provider filter; each
`ToolSummary` carries the canonical schema `name`.
([crates/hya-server/src/v1/catalog.rs](../../crates/hya-server/src/v1/catalog.rs))

## READ

### Canonical schema and path compatibility

The advertised schema is closed and requires only `path`:

```json
{
  "path": "string",
  "offset": "integer >= 1",
  "limit": "integer >= 1",
  "raw": "boolean"
}
```

`filePath` is parsed only as a hidden compatibility field for pre-0.36.9
requests. Runtime resolution keeps `path` and `filePath` distinct: one non-empty
spelling succeeds, equal non-empty values succeed, conflicting non-empty values
return a typed input error, and both missing/empty values fail. Paths are not
trimmed. A legacy `offset: 0` maps to line 1; zero is not advertised.

The workdir is absolutized and lexically normalized. Relative paths join the
workdir; absolute paths remain absolute. `.` is removed and `..` pops one text
component. Symlinks are not canonicalized for the external-directory check, so
a symlink inside the workdir is not external solely because its target is
outside. A lexically external Read authorizes one kind-blind parent wildcard
before metadata, existence, target-kind, normal Read permission, or missing-path
details are observed.

### File kinds and text output

Read dispatches directories, supported media, and text. PNG, JPEG, GIF, WebP,
and PDF files return bounded base64 data-URL attachments. Unsupported binary
files return a typed error. Directory reads list immediate children, put
directories first, sort each group lexically, and use one-based offset paging.

Text removes one leading UTF-8 BOM and normalizes CRLF/lone CR to LF. Non-raw
output uses contextual hashline rows (`LINE#HASH:content`) with stable line
numbers; `raw` returns normalized unanchored text. The terminal empty newline
sentinel is excluded from rendered rows. The default line limit is 2,000 and
the aggregate text budget is 50 KiB; long lines and aggregate truncation carry
bounded notices and a continuation `nextOffset`. Every output also carries
bounded display metadata (`type`, `path`, `text`, `lineStart`, `lineEnd`,
`totalLines`, and `truncated`) for client rendering. Invalid UTF-8 is replaced
with U+FFFD and reported as a warning rather than silently omitted.

([crates/hya-tool/src/read.rs](../../crates/hya-tool/src/read.rs),
[crates/hya-tool/src/hashline/mod.rs](../../crates/hya-tool/src/hashline/mod.rs))

## WRITE

### Canonical schema and result

Write exposes only the closed `{ "path": string, "content": string }` schema.
It keeps hya's existing lexical permission, formatter, LSP, BOM, and whole-file
semantics. Parent directories are created as needed; writes use the shared
same-directory atomic writer, preserve mode/BOM/line-ending behavior, and mark
a leading shebang executable when possible. A chmod failure is a bounded warning,
not a silent failure. Accidental hashline display prefixes are stripped only
when the complete input is unambiguously a rendered hashline block; ambiguous
content remains unchanged.

Formatter and LSP processing run before the final result is built. The returned
`output`, preview, diagnostics, and bounded display metadata therefore describe
the final post-formatter bytes, not the pre-format input. Write also updates the
shared hashline snapshot state so a later Edit can use the same recovery chain.

([crates/hya-tool/src/write.rs](../../crates/hya-tool/src/write.rs),
[crates/hya-tool/src/hashline/fs.rs](../../crates/hya-tool/src/hashline/fs.rs))

## EDIT

### Canonical schema

Edit is a strict hashline operation and exposes only the closed `path + edits`
schema. Every operation object is closed too:

```json
{
  "path": "src/main.rs",
  "edits": [
    { "op": "replace", "pos": "12#KT", "lines": ["new line"] },
    { "op": "append", "lines": ["last line"] },
    { "op": "prepend", "pos": "1#JB", "lines": ["first line"] },
    { "op": "replace_text", "oldText": "old", "newText": "new" }
  ]
}
```

`replace` accepts an optional inclusive `end` anchor and empty `lines` to
delete; `append` defaults to EOF and `prepend` defaults to BOF. `replace_text`
requires exactly one exact occurrence. Literal lines must be file content, not
copied hashline or diff prefixes. The parser rejects unknown fields, malformed
anchors, mixed operation fields, duplicate/conflicting spans, and wrong types
with stable input codes (`E_BAD_OP`, `E_BAD_REF`, `E_NO_MATCH`, `E_MULTI_MATCH`,
`E_STALE_ANCHOR`, ...); each `E_BAD_OP` message names the offending op's actual
required fields and, where relevant, the alternative op to use instead. As a
bounded leniency, `{"op":"replace","oldText":...,"newText":...}` with no
`pos`/`end`/`lines` present is accepted as `replace_text` — that shape can
never validly satisfy `replace` (which requires `pos`+`lines`), so treating it
as the obviously-intended `replace_text` call never masks a real mistake.

An `E_NO_MATCH` failure from `replace_text` additionally scans the file for a
line that matches `oldText`'s most distinctive line after whitespace/quote
normalization; when found, the message and `hints` list point at the
candidate line number(s) (a likely indentation/quoting/line-ending mismatch)
instead of a bare "no match" message. `E_MULTI_MATCH` reports the line numbers
of (at least) the first two occurrences found.

### Anchor validation and recovery

Each anchor is validated against the same pre-edit normalized snapshot. Text
hints can disambiguate a hash collision; hashes are stale-reference aids, never
integrity or authorization data. All spans are resolved before any mutation,
then applied bottom-up. The runtime rejects edits that would make a non-empty
file byte-empty and guards repeated successful payloads and no-op loops.

Only a direct `E_STALE_ANCHOR` failure enters recovery. Stored snapshots are
tried newest-first, and each candidate is merged onto live content with exact
context-three, fuzz-zero hunks. The first exact merge wins; conflicts retain
the stale error plus a recovery note. No fuzzy relocation is attempted. Fresh
anchors, diff metadata, diagnostics, and snapshots are generated from final
post-formatter bytes. The prepared target identity is revalidated immediately
before ordinary rename or hard-link truncate/open, so pathname/alias swaps fail
before mutation. Formatter/LSP failure after mutation reports that the file
changed while retaining the authoritative final snapshot.

The runtime is process-local and bounded by Session, workdir, and resolved
target. It retains at most eight targets, four versions per target, and 32 MiB
of snapshot text. A fixed lock-shard array serializes same-target mutations;
hard-linked aliases share filesystem identity. Cancellation while waiting for
the lock or during execution returns typed `cancelled` without exposing file
contents. After a commit, cancellation first reconciles the actual bytes,
snapshot, and duplicate guard, then returns the typed cancellation.

([crates/hya-tool/src/edit.rs](../../crates/hya-tool/src/edit.rs),
[crates/hya-tool/src/hashline/apply.rs](../../crates/hya-tool/src/hashline/apply.rs),
[crates/hya-tool/src/hashline/merge.rs](../../crates/hya-tool/src/hashline/merge.rs),
[crates/hya-tool/src/hashline/state.rs](../../crates/hya-tool/src/hashline/state.rs))

## GREP

### Canonical schema and search behavior

Grep is native Rust and does not invoke `rg`. `pattern` uses Rust `regex`
crate syntax unless `literal` is true: look-around (`(?=...)`, `(?!...)`,
`(?<=...)`, `(?<!...)`) and backreferences (`\1`) are not supported, and
literal `( ) [ ] { } . + * ? | ^ $` must be escaped. A pattern that fails to
compile reports the `regex` crate's own diagnostic (which names the offending
construct) plus, unless `literal` was already set, a hint to escape the
construct or pass `literal: true` instead. Its closed schema requires
`pattern` and accepts only `path`, `glob`, `ignoreCase`, `literal`, `context`
(0..=5), and `limit` (1..=200) as optional fields. An integer `context` outside
0..=5 is clamped instead of failing the call. Negative values become 0 and
values over 5 become 5. The result says so at the end of its summary line and
as the first `metadata.warnings` entry, for example `3 matches in 1 file.
(context clamped to 5: requested 8, allowed 0–5)`.

```json
{
  "pattern": "TODO|FIXME",
  "path": "src",
  "glob": "*.rs",
  "ignoreCase": true,
  "literal": false,
  "context": 2,
  "limit": 50
}
```

Traversal is cancellable inside directory walking, ignore parsing, line discard,
and matching. It is gitignore-aware, deterministic, and permission-checked for
the search root before metadata probing. Caller globs stop at 4,096 bytes and
both `[!x]` and `[^x]` mean class negation in caller and ignore patterns. Ignore
sources/rule counts are bounded. A logical line over 1 MiB is discarded through
its newline with one bounded warning, then later normal lines remain searchable.
Regex and literal modes honor `ignoreCase`; context ranges are merged and
separated in the result. The worker reads one extra match before setting
`truncated`, so `limit` and `limit + 1` are distinguishable. Matched files are
loaded through the shared text/hashline path, which records snapshots only for
successfully rendered text.

The result keeps the bounded model summary and adds
`metadata.display.groups[]`, where each group is `{path, rows[]}` and each row
contains `{line, text, isMatch}`. Per-file rows are numbered and carry enough
metadata for syntax-aware client rendering without changing the durable Event
model. Grep snapshots enable the same exact stale-anchor recovery path as Read
and Edit.

([crates/hya-tool/src/hashline/mod.rs](../../crates/hya-tool/src/hashline/mod.rs),
[crates/hya-tool/src/grep.rs](../../crates/hya-tool/src/grep.rs))

## APPLY_PATCH

The parameter is `patchText` (serde alias `patch`) carrying a Codex/Compat-style
patch envelope. Supported hunk kinds are **add**, **update**, **delete**, and
**move** (move is an update header plus an optional move line).
([crates/hya-tool/src/apply_patch/mod.rs:16-55](../../crates/hya-tool/src/apply_patch/mod.rs#L16-L55),
[crates/hya-tool/src/apply_patch/parse.rs](../../crates/hya-tool/src/apply_patch/parse.rs),
[crates/hya-tool/src/apply_patch/apply.rs:59-121](../../crates/hya-tool/src/apply_patch/apply.rs#L59-L121))

### Patch envelope grammar

Parsed by `parse_patch` after CRLF → LF normalization:

1. **Sentinels (required):** a line whose trim is exactly `*** Begin Patch`, then
   later a line whose trim is exactly `*** End Patch`. Begin must precede End;
   missing either is an input error (`invalid patch format: …`).
2. **Between the sentinels**, file operations are introduced by headers:
   - `*** Add File: <path>` — body lines until the next file header must **each**
     start with `+` (any other prefix → `add file lines must start with '+'`).
     The `+` is stripped; lines are joined with `\n` and a trailing newline when
     non-empty.
   - `*** Delete File: <path>` — no body.
   - `*** Update File: <path>` — optionally the **very next** line may be
     `*** Move to: <path>` (move destination). Then zero or more update chunks.
3. **Update chunks** start with a line beginning `@@` (optional trailing context
   text after `@@`). Chunk body lines use a one-character prefix:
   - leading space — context (present in both old and new)
   - `-` — removed line
   - `+` — added line  
   A line exactly `*** End of File` ends the chunk and marks end-of-file matching.
4. **Empty / unrecognized body:** if no recognized headers appear between Begin
   and End, the parser returns zero hunks; the tool then rejects with
   `patch rejected: empty patch`.

Example:

```text
*** Begin Patch
*** Add File: notes/hello.txt
+hello
*** Update File: src/main.rs
@@ fn main
-old
+new
*** Delete File: obsolete.txt
*** End Patch
```

Every path in the envelope must be relative and must not escape the session
workdir: an absolute path or a `..` component is an **input error**. Every
touched path (and move destination) is permission-checked as `Action::Edit`
**before** any file is written, so a denial leaves the whole patch unapplied.
([crates/hya-tool/src/apply_patch/mod.rs](../../crates/hya-tool/src/apply_patch/mod.rs))

The result is a Compat-style title plus an aggregate diff and per-file metadata.
After application, the same post-edit formatter + BOM re-sync + LSP-diagnostics
step as write/edit runs for non-delete paths.

As noted under advertisement, `apply_patch` is not model-facing. Models receive
hashline `write`/`edit` instead. The patch envelope remains executable through
the hidden `patch` alias.

## LSP

Operations (exact `operation` enum values):

`goToDefinition`, `findReferences`, `hover`, `documentSymbol`, `workspaceSymbol`,
`goToImplementation`, `prepareCallHierarchy`, `incomingCalls`, `outgoingCalls`.

The call takes a file path (`filePath`) plus 1-based `line` and `character`
(converted to LSP 0-based internally), plus an optional `query` used only by
`workspaceSymbol`. The tool is `ToolPermission::ReadOnly`. It performs an
`Action::ExternalDirectory` check for files outside the workdir, then
`Action::Lsp` on the resolved path. When no language server is registered for the
file type, the tool returns a tool error whose message is
`No LSP server available for this file type.`
([`LspTool`](../../crates/hya-tool/src/lsp.rs),
[`LspOperation`](../../crates/hya-tool/src/lsp_plane.rs),
[`builtin_permission`](../../crates/hya-tool/src/tool.rs))

## Local search: GLOB, FIND, and GREP

### Shared implementation

GLOB, FIND, and GREP use native Rust traversal. GREP does not invoke `rg` or
another external process. Search workers are deterministic and retain only
bounded rows/metadata. GLOB and GREP check cancellation inside traversal work,
not only between completed files. Relative roots resolve against the session
workdir where the tool contract requires it; one kind-blind lexical external
resource is authorized before metadata or target-kind probing.

### GLOB and FIND

GLOB requires `pattern` and optionally accepts a directory `path` (defaulting to
the workdir). Caller patterns stop at 4,096 bytes; `[!x]` and `[^x]` both express
class negation. It returns lexically sorted file paths, capped at 100 rows, with
count and truncation metadata. FIND retains its compatibility-oriented
`{path,size}` result shape and existing unbounded positive result behavior.

### GREP

Grep requires a non-empty `pattern` and accepts only these optional fields:
`path`, `glob`, `ignoreCase`, `literal`, `context` (0..=5; out-of-range
integers are clamped with a note), and `limit` (1..=200).
The input object is closed and rejects unspecified keys.
Regex and literal matching honor `ignoreCase`. Caller and ignore patterns share
negated-class semantics. Traversal bounds ignore sources/rules, skips an
over-budget ignore file with one bounded warning, and continues without
retaining its rules. A file is streamed by logical line; after 1 MiB the worker
discards bytes through the newline without growing the retained line, warns
once, and continues so later matches remain visible. Unsupported/unreadable
files are skipped without exposing their contents, and deterministic file/match
order is preserved.

Context ranges merge when adjacent or overlapping and use separators between
disjoint ranges. Collection observes one additional match before setting
`truncated`, so an exact limit is distinct from `limit + 1`. Matched files are
loaded through the shared text normalizer and hashline formatter; successful
loads update the same Session/workdir/target snapshot history used by Read and
Edit, enabling exact stale-anchor recovery.

The result contains a bounded summary plus
`metadata.display.groups[]`. Each group has a file `path` and bounded `rows`
with `{line, text, isMatch}`. This metadata is a presentation hint, not a new
event or read-model store. It lets a client render a titled block per file with
file-derived syntax highlighting while keeping match identity visible.

([crates/hya-tool/src/grep.rs](../../crates/hya-tool/src/grep.rs),
[crates/hya-tool/src/hashline/mod.rs](../../crates/hya-tool/src/hashline/mod.rs))

## Result envelope and presentation boundary

Every completed builtin coding-tool result keeps the shared `{title, output,
metadata}` envelope. `output` is bounded model-facing text; `metadata` is a
bounded host-facing semantic payload. Read, Write, and Grep expose only bounded
line/file facts, Edit may carry a separately bounded diff and diagnostics, and
Bash exposes bounded command/output status plus truncation and artifact metadata.
The structural cap serializes each nested row/group exactly once and is
idempotent. The engine reapplies it after post-tool hooks, immediately before
durable publication, so neither metadata nor a hook bypasses the final bound.
Provider replay prefers the string `output` member; an object without that
member falls back to serialized JSON.

Clients consume projected `ToolPart` state through the SDK only. (The legacy
TypeScript TUI that implemented this presentation was removed; a future TUI
built on `hya-sdk-v1` should keep the same boundary.) Presentation does not
fetch, poll, replay Events, hydrate a second message store, or schedule a
timer. Completed parts use one allowlisted presentation boundary:

| Tool | Completed presentation |
| --- | --- |
| Read | Titled file/directory block with file-derived syntax highlighting, stable line numbers/offsets, authoritative truncation flags, and bounded collapse. Attachments and directories keep their existing readable fallback. |
| Write | Titled file block with final post-formatter text, syntax highlighting, stable line numbers, first three positioned severity-one diagnostics, and bounded collapse. |
| Edit | Existing semantic diff primitive, with final-state metadata, first three positioned severity-one diagnostics, and distinct narrow unified rows. |
| Grep | One titled block per matched file, numbered context/match rows, explicit match identity, authoritative group/row truncation, and file-derived highlighting. |
| Bash / hidden Shell | One command/output block: nullable exit remains valid for timeout/signal, only the command is syntax-highlighted, output is plain ANSI-stripped text, and textual exit/timeout/truncation status remains. `env` and unknown input keys are excluded. |

Pending, streaming, permission, denied, malformed-data, attachment, directory,
diagnostic, error, and generic fallback states remain on their existing paths.
Malformed or compacted metadata returns to the readable inline/error fallback;
it never renders arbitrary input keys. Local expand/collapse is reversible UI
state and is not persisted as a new Event. At 80 columns Edit uses unified
layout and keeps removed/added rows separate; wide terminals may use split
layout above 120 columns. Replaying a Session through the same SDK projection
produces the same completed blocks.

## Permissions and execution


The registry attaches invocation-level permission metadata to every canonical
name. READ, LS, GLOB, FIND, GREP, LSP, SKILL, LIST_AGENTS, ROSTER, and CHANNELS
are `ReadOnly`; TASK is `Task`; BASH is `Command`; other builtins are general
`Tool` calls. The hidden `shell` alias resolves to the same Bash tool and uses
the same command permission subject. Read-only/task invocations default to
allow, general tool invocations default to ask, and command invocations include
the full command string.
([crates/hya-tool/src/tool.rs](../../crates/hya-tool/src/tool.rs))

Coding-tool permission order is stable: invocation admission runs first. Read
and Grep derive the containing lexical `<dir>/*` scope and authorize it before
metadata/existence/target-kind probing; denied file and directory siblings use
the same resource. Tool-specific permission follows, then filesystem work.
Bash checks command permission before process creation and checks
`ExternalDirectory` for an outside `cwd`. Paths are absolutized and lexically
normalized without symlink canonicalization, preserving the existing symlink
policy. A call-scoped invocation grant never satisfies the separate
external-directory check.

At normal app startup, the action-level snapshot explicitly allows READ, GLOB,
and GREP. Tools still make their own typed action/resource assertions. An
invocation grant satisfies later action checks except `ExternalDirectory`,
which remains independently enforceable under `default`/`strict`. Under
`permission.model: allow`, resource checks (including `ExternalDirectory`)
auto-approve unless a snapshot rule explicitly denies them; `danger` bypasses
checks entirely (including Deny).

The engine processes each model tool call by running plugin before-hooks,
resolving canonical names or hidden aliases, authorizing the invocation,
constructing `ToolCtx`, executing the tool, and running after-hooks. Permission
errors cannot be rewritten by an after-hook. Success is capped before hooks and
again after the final hook replacement, then becomes `Event::ToolResult`;
failure becomes `Event::ToolError` with a structured error value and display
message. Unknown tools and malformed input fail before permission asks. All
coding-tool cancellation paths preserve the typed `cancelled` error.

Tool errors are serialized as `{"error":{"type":...,"message":...}}` with
these wire `type` strings:

| Variant | Wire `type` |
| --- | --- |
| `Input` | `input` |
| `Permission` | `permission` |
| `Io` | `io` |
| `Json` | `json` |
| `Cancelled` | `cancelled` |
| `Overloaded` | `overloaded` |
| `OperationIdConflict` | `operation_id_conflict` |
| `OperationAlreadyHandled` | `operation_already_handled` |
| `WorkflowControl { code, message }` | the control `code` itself (e.g. `WORKFLOW_BUSY`) |
| `UnknownAgentId` | `unknown_agent_id` |
| `AgentSpawnNotAllowed` | `agent_spawn_not_allowed` |
| `UnsupportedInlineAgentField` | `unsupported_inline_agent_field` |
| `Other` | `unknown` |

Only `permission` is protected from rewriting by `tool.execute.after` hooks.

## Runtime planes and extensions

All builtin schemas are registered before runtime capabilities are considered.
`ToolCtx` carries permission, interaction, spawner, mailbox, todo, skills, web
search, LSP, formatter, workdir, session, and cancellation planes/resources,
plus an immutable caller-reachable `AgentDef` roster derived from the bound
agent's `can_spawn` reachability (not a mutable agent catalog plane). The
single `BundleCatalog` authority lives on `RuntimeSnapshot` / `TurnBinding`;
application wiring does not replace an agent catalog authority.
([`ToolCtx`](../../crates/hya-tool/src/tool.rs),
[`AgentDef`](../../crates/hya-tool/src/agents.rs),
[`RuntimeSnapshot`](../../crates/hya-core/src/runtime_registry.rs),
[`TurnBinding::bundle_catalog`](../../crates/hya-core/src/runtime_registry.rs),
[`build_session_engine`](../../crates/hya-app/src/runtime.rs))

A bare `SessionEngine` starts with a disconnected mailbox and default
interaction, spawner, todo, skill, websearch, formatter, and LSP planes. The
application replaces the interaction, spawner, mailbox, and formatter planes
and starts the mailbox service. Agent discovery for tools uses the immutable
per-turn `AgentDef` roster from the bound catalog's `can_spawn` set rather than
an injectable catalog plane. Consequently, registry presence alone does not
prove that a plane-backed tool can return useful data; for example, mailbox
operations report that they are available only inside a running team, and LSP
reports when no server supports a file type.
([`SessionEngine::new`](../../crates/hya-core/src/engine.rs),
[`build_session_engine`](../../crates/hya-app/src/runtime.rs),
[`MailboxError::Unavailable`](../../crates/hya-tool/src/mailbox.rs),
[`LspTool`](../../crates/hya-tool/src/lsp.rs))

### MCP tools

At startup or through the v1 MCP control routes (`POST /v1/mcp`,
`POST /v1/mcp/{name}/connect`, `POST /v1/mcp/{name}/disconnect`), hya prepares
enabled MCP servers and adapts tools returned by `tools/list`. Disabled or
failed servers contribute no new tools. A complete current-revision candidate
is published for the next turn; an older bound turn keeps its retained source
client and view. Only MCP tools whose input schema has `type: "object"` are
accepted.
([crates/hya-mcp/src/manager.rs:59-100](../../crates/hya-mcp/src/manager.rs#L59-L100),
[crates/hya-mcp/src/manager.rs:105-140](../../crates/hya-mcp/src/manager.rs#L105-L140),
[crates/hya-mcp/src/bridge.rs:20-43](../../crates/hya-mcp/src/bridge.rs#L20-L43))

The model-facing name is `mcp__{server}__{tool}`, while execution sends the
remote tool's original name in `tools/call`. MCP adapters assert `Action::Mcp`
and are registered with `ToolPermission::Mcp`. Text and supported image/PDF
content is normalized into hya output and attachments.
([crates/hya-mcp/src/bridge.rs:36-80](../../crates/hya-mcp/src/bridge.rs#L36-L80),
[crates/hya-mcp/src/bridge.rs:83-103](../../crates/hya-mcp/src/bridge.rs#L83-L103),
[`prepare_mcp_results`](../../crates/hya-app/src/runtime.rs))

### Plugin tools

Connected plugins contribute declared tools whose input schema has
`type: "object"`. Plugin tool names are preserved as declared rather than
namespaced, execution requires a session, and calls are forwarded to the
owning plugin. They are registered as general `ToolPermission::Tool` tools.
([crates/hya-plugin/src/plugin_tool.rs:18-34](../../crates/hya-plugin/src/plugin_tool.rs#L18-L34),
[crates/hya-plugin/src/plugin_tool.rs:36-58](../../crates/hya-plugin/src/plugin_tool.rs#L36-L58),
[crates/hya-plugin/src/host.rs:387-397](../../crates/hya-plugin/src/host.rs#L387-L397),
[`prepared_plugin_results`](../../crates/hya-app/src/runtime.rs))

Registry names are unique across builtins, MCP tools, plugin tools, and their
aliases. Any duplicate source, configured/handshake plugin-ID mismatch,
same-source duplicate export, or canonical/alias collision rejects the whole
candidate before generation allocation; the previous effective snapshot stays
active. There is no insertion-order overwrite.
MCP namespacing reduces MCP collisions, while unnamespaced plugin declarations
can collide directly with a builtin or another plugin.
([`ToolRegistry::register_with_permission_and_aliases`](../../crates/hya-tool/src/tool.rs),
[`build_session_engine`](../../crates/hya-app/src/runtime.rs))

## Provenance

Read, Edit, and Grep behavior follows `pi-hashline-edit` 0.8.3, pinned by npm
git head `ba7db9943d0f58499b24c1f6bd64722580f772a5` and tarball SHA-1
`8985f24c3493be375cc225a5522ed54de8daabc9`. Write and Bash are host contracts
aligned with `@oh-my-pi/pi-coding-agent` 18.1.3 at
`can1357/oh-my-pi@0b769cc4dd9771373335430385d1d2f696dc3498`. The Rust
implementation is native and does not add a JavaScript runtime dependency;
license notices are shipped with the source-derived implementation.

## Verified boundaries

The focused contracts are owned by these seams:

- Native schemas and adapters: [`crates/hya-tool/src/read.rs`](../../crates/hya-tool/src/read.rs), [`write.rs`](../../crates/hya-tool/src/write.rs), [`edit.rs`](../../crates/hya-tool/src/edit.rs), [`grep.rs`](../../crates/hya-tool/src/grep.rs), and [`shell.rs`](../../crates/hya-tool/src/shell.rs).
- Shared hashline formatting, strict operations, exact recovery, atomic writes,
  snapshots, and lock bounds: [`crates/hya-tool/src/hashline/`](../../crates/hya-tool/src/hashline/).
- Invocation/resource permission and typed error mapping:
  [`crates/hya-tool/src/permission.rs`](../../crates/hya-tool/src/permission.rs)
  and [`crates/hya-core/src/engine/tool_error.rs`](../../crates/hya-core/src/engine/tool_error.rs).
- Durable result projection and provider replay: [`docs/architecture/event-model.md`](event-model.md).

These boundaries describe shipped behavior, not a promise of full Compat
superset behavior. Historical 0.36.8 `ToolError` Events remain immutable and
visible on replay. An already-running 0.36.8 backend must restart before new
calls use the 0.36.9 schemas and runtime.
