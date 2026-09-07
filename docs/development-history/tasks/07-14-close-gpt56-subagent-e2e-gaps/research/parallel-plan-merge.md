# Parallel Plan Merge

## Inputs

- Conservative/minimal planner: reuse prior passes, run zero-cost current-source
  TUI diagnosis, then one resident team and nested last; recommended 22 outbound
  requests and 20 minutes.
- Risk/failure planner: enforce the cap before HTTP forwarding, pre-title roots,
  retain separate direct/channel wakes, and distinguish current source from the
  installed runtime; recommended 20 outbound requests and 30 minutes.

## Agreements

- No prior live-call authorization carries into this task.
- Prior passing slices must not be rerun.
- Pre-`StepStarted` provider 5xx is an external attempt, not a local governor or
  spawn defect.
- Current-source TUI behavior must be tested without paid traffic before any
  product edit.
- TUI automation uses the effective `Ctrl+X`, then `Down` child binding and `Up`
  parent binding, not the unimplemented manager chords in ADR-0003.
- Product source changes require a deterministic behavior-contract RED first.
- Canonical events and projection are authoritative; model prose is not.

## Resolved Disagreements

### Request And Time Cap

Chosen: 20 outbound requests and 30 minutes. The lower request ceiling minimizes
spend; the longer clock accommodates the prior 130-second upstream timeout.
Requests are counted before forwarding by a private localhost relay, including
failed and automatic requests. Request 21 is rejected locally.

### Slice Order

Chosen: zero-cost TUI diagnosis, resident/mailbox/live TUI, then nested. Resident
and TUI are untouched coverage gaps. Nested is last because another slow 524
must not prevent collection of independent evidence.

### Resident Mail Contract

Chosen: preserve separate direct-mail and channel-mail wake/reply cycles from
the original PRD. Wait for canonical idle after direct mail before channel mail
so the two wakes cannot coalesce.

### TUI Contract

Chosen: require child transcript/status, an explicit visible read-only marker,
no Prompt composer, ignored ordinary text, and a preserved main draft. Current
source lacks the explicit marker in `SubagentFooter`; this remains a RED
candidate until the PTY behavior test runs.

## Budget Allocation

| Slice | Hard Maximum |
| --- | ---: |
| Resident, mailbox, quiescence, and live TUI | 10 requests |
| Nested, including at most two attempts | 10 requests |
| Total | 20 requests |

One action-specific rewritten prompt is allowed only within the owning slice's
remaining allocation. No passing action is repeated.

## Evidence Matrix

| Contract | Canonical evidence |
| --- | --- |
| Resident spawn | Correlated `task` call/result, child ancestry/route, `AgentRegistered`, stable handle, immediate running outcome |
| Direct wake | Direct `MailSent`, resident active/idle pair, nonce reply to main |
| Channel wake | `ChannelJoined`, channel membership, channel `MailSent`, second active/idle pair, distinct nonce reply, `ChannelLeft` |
| Quiescence | Exactly one main synthesis wake after final idle and no repeat without new work |
| TUI read-only | Child frame with transcript/status/read-only marker, no composer, ignored child sentinel, preserved root draft, unchanged prompt events |
| Nested | Root/child/grandchild ancestry and exact route, child nested `task`, complete member lifecycle, grandchild nonce propagated to root |
| External block | Provider status anchored before `StepStarted`, with no local tool/permission event for that attempt |

## Hard Stops

- Unexpected permission, credential output, auth/config mutation, request 21,
  30-minute deadline, or an untracked task-owned process after cleanup begins.
- Two nested pre-tool provider failures close only nested as externally blocked.
- A normal response omitting a requested tool is model adherence; it does not
  authorize source edits.
