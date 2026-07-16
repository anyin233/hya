# Close GPT 5.6 subagent E2E gaps

## Goal

Close the three remaining GPT 5.6 Sol subagent verification gaps with
canonical runtime evidence, while distinguishing an upstream provider failure,
an automation gap, and a reproducible product defect before changing source.

## Background

- The prior installed-runtime run used `hya-backend 0.33.1`, real `hya-ts`
  PTYs, and `12th-oai/gpt-5.6-sol` for every root and child session.
- Discovery, foreground spawn, same-session resume, parallel category/inline
  spawn, and non-blocking background spawn passed with SQLite event evidence.
- The nested slice reached its depth-1 child, then the provider returned HTTP
  524 before `StepStarted` or any nested tool call. This establishes an
  upstream transport failure for that attempt, not a governor deadlock.
- Resident/mailbox behavior and full TUI observation/input isolation were not
  run after that stop condition. They are coverage gaps, not known defects.

## Requirements

### R1. Bound the live run

- Obtain approval for the recommended hard cap of 20 outbound provider requests
  and 30 minutes before issuing paid model requests. The previous task's cap is
  not reusable.
- Count every attempted outbound request at a private localhost relay before it
  is forwarded, including failed and automatic requests; reject request 21.
- Pre-title every root session before its first prompt so automatic title
  generation cannot silently spend a provider request.
- Use the exact `12th-oai/gpt-5.6-sol` route, depth `2`, concurrency `2`, spawn
  budget `8`, resident turn budget `16`, and message budget `12`.
- Do not use `--yolo`. Approve only the expected `task` permission; stop on any
  unrelated permission request.
- Reuse already-passing evidence rather than rerunning completed slices.

### R2. Close nested-spawn evidence

- Drive one depth-1 child to invoke `task`, create one depth-2 grandchild, and
  return a nonce-bearing result through the parent.
- Prove admission, parentage, exact model route, member lifecycle, grandchild
  completion, and correlated result from canonical events.
- Treat a 5xx response before `StepStarted` as external evidence, not a local
  defect. Retry only within the approved cap and do not add provider retry
  behavior without a separate reproduced requirement.

### R3. Close resident and mailbox evidence

- Register one resident subagent and prove non-blocking spawn, stable handle,
  idle state, one mail-triggered wake, one completed turn, and return to idle.
- Exercise and verify `roster`, direct `send`, `join`, `channels`, channel
  `send`, and `leave` using team-root events and replayed projection state.
- Prove direct mail and channel mail each cause a separate nonce-bearing
  idle/active/idle resident cycle and reply to main; wait for idle between sends
  so the wakes cannot coalesce.
- Prove quiescence wakes the main agent once without exceeding turn or message
  budgets.

### R4. Close TUI observation evidence

- Use a real installed `hya-ts` PTY and the current configured subagent
  observation controls (`Ctrl+X`, then `Down`; `Up` returns to the parent), not
  the newer ADR manager chords that this frontend does not implement.
- Open a live child observation, prove child transcript/status and read-only
  marker are visible, and prove no Prompt composer is exposed there.
- Type ordinary text while the observation is focused and prove it neither
  mutates nor submits the main session prompt; return to the main view and
  confirm normal input remains available.

### R5. Fix only confirmed local defects

- For a deterministic local failure, first reproduce the exact symptom against
  current source at the owning behavior boundary and observe one focused RED
  test.
- Apply the smallest root-cause fix, then rerun the focused test and original
  E2E slice.
- Any fix or feature source change must include the project-required version
  and changelog update and pass the complete touched-area verification gate.
- If current source already passes, record installed-version drift rather than
  changing code.

### R6. Preserve security and workspace hygiene

- Never print or persist auth tokens. Preserve auth/config permissions and
  verify the auth file hash is unchanged.
- Keep unrelated dirty and untracked files untouched.
- Stop all spawned PTYs/backends and remove only this run's private temporary
  artifacts.

## Acceptance Criteria

- [ ] A new paid-call/time cap is recorded before the first live request.
- [ ] Nested spawn either passes with canonical depth-2 evidence or is recorded
      as externally blocked with the exact pre-tool provider boundary.
- [ ] Resident registration, idle/wake/idle, mailbox operations, and one-shot
      quiescence are proven from canonical team-root events and projection,
      including separate direct-mail and channel-mail wake/reply cycles.
- [ ] A real TUI observation view is opened and ordinary text is proven ignored
      without changing or submitting the main prompt; a visible read-only marker
      and the absence of the Prompt composer are captured.
- [ ] Every reproducible local defect has one observed RED test, a minimal fix,
      GREEN focused/original checks, and required version/changelog alignment.
- [ ] If no local defect is reproduced, no product source or version is changed.
- [ ] Final evidence lists request usage, session IDs/event ranges, commands,
      pass/block classification, and cleanup checks without secrets.
- [ ] All task artifacts validate and unrelated workspace changes remain
      untouched.

## Out Of Scope

- Revalidating the already-passing discovery, foreground, resume, parallel,
  category/inline, or background slices.
- Adding generic provider retries or masking upstream 5xx responses.
- Introducing a new E2E framework when existing PTY, HTTP, SQLite, and package
  test seams can prove the contracts.
- Allowing direct user input to resident or transient subagents.

## Notes

- Prior evidence: `.trellis/tasks/07-14-e2e-gpt56-sol-subagents/e2e-results.md`.
- The only blocking product decision is the fresh paid provider request/time
  cap; all other planning questions are repository-answerable.
- The recommended cap is 20 outbound requests and 30 minutes, enforced before
  forwarding rather than inferred from canonical turn events.
