# Design — Track P scenarios for swarm tools

## What the investigation changed

The PRD assumed the main obstacle was FakeLlm scripting. That is real, but it is
the *second* problem. Two findings reshape this task:

### 1. Mail delivery only exists for **resident** agents

`crates/hya-core/src/resident.rs` is where an inbox actually reaches an agent:
messages are injected into a resident handle's context, tracked by an
`inbox_cursor` ("how many of this handle's inbox messages have already been
injected"). `SubagentMode` has exactly two variants — `Transient` and
`Resident` — and every existing Track P scenario spawns **transient**
background subagents.

So a `send` → *delivered* oracle requires at least one **resident** teammate
that takes a turn after delivery. No current Track P scenario does this, and the
harness has never spawned a resident. This is the real cost of the task, not the
test files.

### 2. The mailbox has **no HTTP surface**

`grep` over `crates/hya-server/src` finds no mailbox/roster route. So a test
cannot read mailbox state over HTTP the way it reads `/session/{id}/tree` or
`/session/{id}/todo`. Everything must be observed through one of:

| Observation channel | What it can prove |
| --- | --- |
| Recipient's **next FakeLlm request body** | that a message was actually *injected into the recipient's context* — the strongest available oracle |
| Caller's **tool result**, visible in the caller's follow-up FakeLlm request | what `roster` / `channels` reported |
| `/session/{id}/tree` | that the teammates exist as sessions |

The PRD's R2 ("assert on the recipient's observable state, never the sending
tool's own success string") therefore maps to: **assert on the recipient's
follow-up FakeLlm request**. That is the only honest delivery proof available.

## FakeLlm must become per-agent routable

`src/fake_llm.rs` holds one `VecDeque<ScriptStep>` and `chat_completions`
pops the front for *any* request. With two live agents the pops interleave
nondeterministically, so agent A can consume agent B's script. Every
multi-agent scenario would be flaky by construction.

**Design: additive routing, default queue preserved.**

```rust
pub struct FakeLlm { /* … */ }

/// Route requests whose body contains `marker` to a dedicated queue.
pub fn route(&self, marker: impl Into<String>, steps: Vec<ScriptStep>);
```

- `Shared` gains `routes: Vec<(String, VecDeque<ScriptStep>)>` alongside the
  existing `scripts` queue.
- `chat_completions` serializes the incoming body once, finds the **first**
  route whose `marker` is a substring and whose queue is non-empty, and pops
  from it; otherwise falls back to `scripts` exactly as today.
- With no routes registered the behavior is byte-identical, so the existing 19
  scenarios cannot be disturbed. That property is the reason for routing by
  fallback rather than replacing the queue.

Markers are agent-identifying strings that appear in the request — the agent's
system prompt text is the natural choice, since each roster agent has a distinct
prompt body. The marker must be chosen from content the harness itself writes
(a skill/agent prompt it created), not from incidental model text, or the
routing becomes as fragile as the thing it replaces.

## Scenario shape

One file, `tests/p16_swarm_mailbox.rs`, with a shared fixture that spawns a
resident teammate and then exercises each tool. Splitting across six files would
mean paying the resident-spawn setup six times, and Track P runs serially.

| ID | Tool | Oracle |
| --- | --- | --- |
| T2.4 | `roster`, `list_agents` | caller's follow-up request contains the teammate's handle and agent type |
| T2.5 | `send` (direct) | **recipient's** next request body contains the message body marker |
| T2.6 | `send` (`#channel`) | recipient's next request body contains it; receipt's `recipients` is non-empty |
| T2.9 | `channels` | follow-up shows the channel with its member/message counts |
| T2.10 | `join` | after joining, a channel `send` reaches the joiner (recipient-side proof) |
| T2.11 | `leave` | after leaving, a channel `send` **does not** reach it — negative proof per PRD R3 |

T2.4–T2.6 reuse the numbering gap the parent PRD flagged as undefined; child 5
owns formally retiring or defining the rest. Coordinate before landing.

## The `leave` negative oracle is the hard one

"Message did not arrive" is unfalsifiable against an arbitrary wait. The test
must bound it with a *positive* control in the same run:

1. Joiner leaves the channel.
2. A second, still-subscribed member remains.
3. Send one channel message.
4. Wait until the **still-subscribed** member's request shows the message —
   that is the "delivery has happened" clock.
5. Only then assert the departed member's request history never contained it.

Without step 4 the test proves nothing except that it did not wait long enough.

## Risks

| Risk | Mitigation |
| --- | --- |
| Resident agents may not terminate, hanging the suite | Bound every wait; ensure the fixture shuts the backend down in `Drop` as the existing harness does |
| Routing markers match the wrong request | Assert the marker appears in exactly the intended agent's requests before relying on it; fail loudly if a route is never consumed |
| Resident spawn may need admission preconditions like child 1 hit | `crates/hya-core/tests/resident.rs` and `resident_recovery.rs` already spawn residents in-process — read them first for the required fixture shape |
| The 19 existing scenarios regress | The routing change is fallback-only; run the full Track P suite before and after |

## Explicitly not in this design

- No product changes to the swarm tools. If a scenario exposes a real bug, it
  gets filed, not fixed here (PRD "Out of scope").
- No failure paths (send to unknown handle, join nonexistent channel).
- The remaining uncovered tools (`ls`, `glob`, `grep`, `lsp`, `apply_patch`,
  `webfetch`, `websearch`, `plan_exit`) stay uncovered.
