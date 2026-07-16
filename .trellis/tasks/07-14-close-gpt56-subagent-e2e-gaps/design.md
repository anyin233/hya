# Design

## Evidence Boundary

The task has three independent verification slices. Each slice is classified
before source changes:

1. Pass: canonical events and projection satisfy the contract.
2. External block: the provider fails before local tool/runtime behavior starts.
3. Automation gap: product state is correct but the PTY driver or assertion is
   wrong; correct the driver without changing product behavior.
4. Product defect: current source deterministically violates the documented
   contract; establish a RED behavior test before fixing it.

Model prose is never acceptance evidence. Session ancestry, routes, lifecycle,
mail, channels, and quiescence come from the team-root event log and replayed
projection. TUI input isolation additionally requires visible PTY state plus
unchanged canonical prompt events.

## Execution Shape

### Preflight

- Use a fresh private runtime directory and SQLite database.
- Point a disposable provider config at a private localhost counting relay and
  reference the existing auth without reading or logging its secret value.
- Record binary versions, exact model listing, config/auth modes, auth hash,
  limits, and the approved request/time cap.
- Count every outbound request before forwarding at the relay; log only ordinal,
  monotonic time, path, and status. Reject requests beyond the approved cap.
- Create roots with explicit titles before prompting to suppress automatic
  title-generation requests.

### Zero-Cost TUI Diagnosis

Before paid traffic, run current source against the existing dev provider and a
parent/child fixture. Use the effective `Ctrl+X`, then `Down` binding and `Up`
to return. Preserve a root draft, type a distinct child sentinel, and assert the
child route has transcript/status/read-only marker but no Prompt composer or new
prompt request/event. This distinguishes route behavior from PTY timing and
installed-version drift.

### Resident, Mailbox, And TUI Slice

Use one resident team for all remaining contracts to minimize paid calls:

1. Main spawns one resident and receives its handle without blocking.
2. Main reads roster, joins a nonce channel, and reads channels.
3. Main sends direct mail and waits for the resident's nonce reply and return to
   idle before sending channel mail.
4. Channel mail causes a second nonce reply and idle/active/idle cycle; only
   then does the resident leave the channel.
5. Quiescence wakes main once for synthesis.
6. While the resident is live, the same backend is observed through a real
   `hya-ts` PTY to test navigation and read-only input isolation.

If combining all mailbox actions in one model instruction proves unreliable,
split only the failed action into another bounded turn. Do not rerun passing
actions.

### Nested Slice

Run nested last because it is the only slice previously blocked by a slow
provider 524. Use unique nonces at root, depth 1, and depth 2. Assert the
depth-1 child emits a nested `task` call before expecting a grandchild. Permit
at most two attempts and no more than 10 of the 20 forwarded requests for this
slice. A provider 5xx before `StepStarted` remains external; it does not justify
transport retry code.

## Defect Branch

Live E2E identifies the symptom; current source owns the fix decision. Before
editing:

- Reproduce on current source using the narrowest existing boundary-owned
  suite: `hya-core`/`hya-app` for nested or resident orchestration,
  `hya-tool`/`hya-proto` for mailbox contracts, and the TypeScript TUI harness
  for observation input/navigation.
- Rank falsifiable hypotheses and use one targeted probe at a time only when the
  focused test does not already isolate the cause.
- Add one regression test that fails for the exact behavior, observe RED, apply
  the minimum fix, and observe GREEN.
- Update workspace version and the single-version root changelog only when
  product source changes.

No compatibility layer, new abstraction, or generic retry mechanism is added
without a reproduced contract requiring it.

## Failure And Stop Rules

- Unexpected permission: stop immediately.
- Auth/config mutation or credential exposure: stop and clean up.
- Approved request or wall-clock cap reached: stop, preserve canonical evidence,
  and report the remaining gap.
- Repeated provider 5xx before local execution: classify external block and do
  not edit source.
- A normal model response that omits a requested tool is model adherence, not a
  product defect; permit at most one tighter action-specific prompt within cap.
- A failed PTY chord without matching route/state evidence: inspect current
  keymap and driver timing before classifying a product defect.

## Rollback And Cleanup

Product edits, if any, remain isolated from pre-existing dirty files and can be
reverted by their exact file set. Runtime cleanup always stops child processes,
the relay, and the watchdog before deleting this task's private database and
config. The user's auth and config files are never replaced during cleanup.
