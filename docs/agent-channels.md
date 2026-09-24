# Agent channel bundle

## Introduction

An agent channel bundle is an `AgentSetBundle` with declarative `channels[]`
policy. It describes channel topology and capability ceilings for agents while
leaving concrete channel creation to the engine. Runtime channel ids such as
`announce-…` and `DM-…` are always minted as event facts and never appear in a
bundle.

The trusted `hya/agent-channels` preset (a [first-party bundle](bundle-runtime.md#first-party-bundles)) records the existing behavior. A
manifest without `channels` continues to use that trusted default policy when
the engine integration consumes this contract. Installed declarations can only
intersect with and restrict those grants; they cannot elevate engine or preset
permissions. A present template with an empty `capabilities` list grants no
optional channel capability.

## Usage

Declare templates in an `AgentSetBundle`:

```yaml
kind: AgentSetBundle
identity: { id: acme/review-channels, version: 1.0.0, publisher: acme }
agents:
  - { id: reviewer, role: subagent }
channels:
  - id: review-parent
    kind: parent_dm
    participants:
      - { kind: role, role: parent }
      - { kind: agent, agent: reviewer }
    capabilities: [send, report]
    scope: vertical
    retention: team_session
```

An AgentSetBundle may omit `agents` or use `agents: []` only when it declares at
least one valid channel template. This supports standalone channel policy
bundles such as `hya/agent-channels`. Agent references are local ids from the
same manifest; topology roles let channel-only bundles remain independent of a
specific roster.

### Channel-aware `wait`

The channel tool family (`hya/channel-tools`: `send`, `list_channel`,
`report`, `wait`) owns the mail tools. When it is loaded — always, in the
default builtin registry — its `wait` replaces the member-only `wait` of
`hya/extended-tools` through an explicit `overrides: hya/extended-tools`
declaration in its exposure policy ([Tool-family presets](base-tools.md#overrides)).
Besides returning when subagents finish (they report or are archived — idle
is never finished), it returns as soon as new mail reaches the caller's inbox —
on a DM or group channel, or harness mail such as the `LEADER FAILED` wrap-up
notice, which bypasses channel policy — with `woke_by: "mail"` and each
message's body (bounded to 600 chars, like the `[NEW MAIL]` notice):

```json
{"woke_by": "mail", "mail": [{"from": "harness", "preview": "LEADER FAILED: …"}], "finished": [], "running": [], "waited_ms": 812}
```

Mail is returned **once**. "New" means past the caller's durable inbox cursor
(the `MailConsumed` cursor that in-turn steering and resident wakes share), and
a returning `wait` advances that cursor through the mail it returned — so the
same message never comes back from a later `wait`, never reappears in a
`[NEW MAIL]` notice, and never re-wakes the caller as a user prompt. A
subagent's report mail is delivered as that subagent's finish (with its
report under `finished`), not as a mail wake.

A subagent with no subagents of its own can call `wait` to block until its
parent (or the harness) writes to it.

### Every agent gets the mail tools

When the channel family is loaded, the harness allocates `send`,
`list_channel`, `wait`, and `read channel://<id>` (plus `report` for
subagents) to **every** agent at startup — built-in or bundle, root or member —
whatever its bundle `resource_view` selects. A view that does not select `read`
gets a mail-only `read` that serves `channel://` handles and refuses file
paths. So a narrow bundle subagent can always read the mail its report gate
names; the gate error says which channel and how:

```text
report rejected: `main/scout-1` has 1 unread mail message(s) on #DM-rgli51cb (1); answer them first. Read it with `read channel://DM-rgli51cb` (`list_channel` lists every channel with unread counts), reply with `send` if the sender needs an answer, then call `report` again — or call `wait` to block until more mail arrives.
```

Mail also reaches a working agent as a `[NEW MAIL]` notice appended to its next
successful tool result. The notice is rebuilt from the durable inbox past the
durable cursor whenever team mail was sent (or the live event bus overflowed),
with the agent's channel view refreshed — so no mail is dropped, mail on a
channel created after the turn began (a new subagent's DM) is included, and
mail a `wait` already returned is not repeated. See
[Agent tool surface](architecture/agent-tool-surface.md#coordination-tools-allocated-at-startup)
for the full allocation table and `deny` rules. Without the channel family, `wait` does
not wake on mail; mail still arrives in the `[NEW MAIL]` notice after the next
tool call. See [Agent tool surface](architecture/agent-tool-surface.md) for the
full `wait` contract.

## Interface definitions

Each `channels[]` entry is closed and has these fields:

| Field | Type | Contract |
| --- | --- | --- |
| `id` | string | Required unique lowercase identifier using letters, digits, `_`, or `-`. This is a template id, never a runtime channel id. |
| `kind` | enum | `unit` or `parent_dm`; each kind may appear at most once per bundle. |
| `participants` | array | Required nonempty unique selectors. Each is `{ kind: agent, agent: <local-id> }` or `{ kind: role, role: <role> }`. |
| `capabilities` | array | Optional unique subset of `send`, `report`, `steer`, `follow_up`, `resident_mail`; omitted or empty denies all optional capabilities. |
| `scope` | enum | `unit` for `unit`; `vertical` for `parent_dm`. Mismatches reject. |
| `retention` | enum | Required `team_session`, the current durable event-log lifetime. |

`unit` maps to the existing `ChannelKind::Group`: one unit leader and its direct
reports. This is the runtime's broadcast pipe, so there is no separate
`broadcast` kind. Its role selectors are `unit_leader` and `direct_reports`.

`parent_dm` maps to the existing `ChannelKind::Dm`: one vertical parent-child
pair. Its role selectors are `parent` and `child`. Arbitrary peer direct-DM
templates are not supported by the runtime and therefore have no schema kind.

The capability list is a restrictive upper bound:

- `send` permits ordinary mailbox sends.
- `report` permits terminal child reporting through a parent DM.
- `steer` permits unread mail to enter an active turn.
- `resident_mail` is the base permission for delivery to a resident member.
- `follow_up` permits that resident delivery to schedule another model turn.
  Resident delivery requires both capabilities. This conjunction keeps live
  delivery and restart recovery equivalent because arrival-time busy/idle state
  is not a durable mailbox fact.

Preparation sorts templates, participants, and capabilities, rejects duplicate
values, validates local Agent references and kind-specific roles, and includes
the canonical result in the bundle digest. Prepared-catalog decoding repeats
the validation, including when bytes carry a freshly recomputed outer digest.

The engine consumes the policy at mailbox send and report admission, steer
mailbox delivery, follow-up scheduling, and resident wake admission. Existing
registration continues to mint the unit and parent-DM `ChannelCreated` events;
templates never supply those ids. The bundle contract does not create sessions,
channels, mailbox events, or recipients itself.

Each admitted session retains its channel-policy snapshot for direct engine API
calls as well as tool calls. A send without a captured snapshot fails closed.
Mail excluded from model delivery is durably marked consumed: it does not wake a
resident, reappear after recovery, or leave the terminal report gate blocked.
After a process restart, resident recovery captures policy from the currently
resolvable bundle generation. An uninstalled or otherwise unavailable bundle
cannot be reconstructed from stale catalog metadata; recovery then leaves the
actor inert, and direct send or steer entry points fail closed until a real
binding is admitted.
