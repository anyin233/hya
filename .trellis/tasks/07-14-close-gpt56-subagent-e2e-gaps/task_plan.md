# Task Plan

## Goal

Close nested, resident/mailbox, and TUI GPT 5.6 Sol E2E gaps with bounded
canonical evidence; fix only deterministic current-source defects.

## Phases

| Phase | Status | Exit Condition |
| --- | --- | --- |
| 1. Evidence and planning | complete | Parallel plans merged; artifacts and manifests validate |
| 2. Approval and zero-cost TUI diagnosis | complete | Plan/cap approved; current-source behavior classified |
| 3. Bounded live E2E | complete | Fresh TUI and nested passed; final mailbox retry preserved bounded partial evidence |
| 4. Conditional TDD fix | complete | Confirmed TUI defect has RED, minimal fix, and GREEN |
| 5. Verification and hygiene | complete | Required gates pass; evidence and cleanup complete |
| 6. Handoff | complete | Final evidence and scoped diff reviewed; bounded gaps recorded |

## Decisions

- Reuse prior passing evidence; do not rerun completed slices.
- HTTP 524 before `StepStarted` is external evidence for that attempt.
- Recommended cap: 20 forwarded requests and 30 minutes, enforced at a private
  relay; pre-title roots to prevent hidden title requests.
- Run zero-cost current-source TUI diagnosis first, then combine
  resident/mailbox/TUI on one live team, then run nested last.
- Require separate direct-mail and channel-mail resident wake/reply cycles.
- Use effective TypeScript bindings `Ctrl+X`, then `Down`; `Up` returns to main.
- No product source, version, or changelog change without a reproduced local
  defect on current source.
- No live provider request until a new cap is explicitly approved.
- The approved cap is 20 forwarded requests and 30 minutes; 10 requests are
  reserved for resident/mailbox/TUI and 10 for at most two nested attempts.
- The confirmed TUI defect is fixed by rendering `Read-only` in the existing
  subagent footer; no new observation state or input path is needed.
- The user authorized one fresh 30-minute retry using only the 16 requests left
  from the original total: 6 for resident/live TUI and 10 for nested. This does
  not increase the task's 20-request aggregate ceiling.
- Tighten resident instructions. Approve only the root resident-spawn `task`;
  any child `task` permission remains an immediate stop.
- Fresh resident/TUI retry used its full 6-request allocation. Aggregate usage is
  `4 + 6 = 10`; nested received no request from this retry.
- For nested only, the user authorized exactly two correlated `task`
  permissions: root-to-depth-1 and depth-1-to-depth-2. Any third `task` or any
  unrelated permission is an immediate stop. Nested retains 10 requests and a
  fresh 30-minute window.
- Corrected relay framing was proven locally at zero provider count. Nested then
  passed on attempt 1 with 5 requests. Aggregate usage is `4 + 6 + 5 = 15 / 20`.
- The user explicitly increased the ceiling for one final mailbox-only retry:
  up to 12 additional requests and 30 minutes. Aggregate usage may therefore
  reach 27. No TUI or nested rerun is authorized.
- Final mailbox forwarded all 12 authorized requests successfully; fresh attempt
  13 was rejected locally. Aggregate usage is `27 / 27`; no further live request
  is authorized.
- Canonical role-correlated replay, not raw text matching, is authoritative for
  completion nonces because `text_delta` does not itself carry message role.

## Blockers

- No nested or live-TUI blocker remains. The final mailbox retry passed direct
  delivery/reply and idle evidence but exhausted its cap before channel
  send/leave and completed final synthesis. No additional live provider request
  is authorized.

## Errors Encountered

| Error | Attempt | Resolution |
| --- | --- | --- |
| Prior nested request returned HTTP 524 | Previous task, one attempt | Preserve as upstream-boundary evidence; bounded retry only in this task |
| Initial PTY readiness predicate timed out | Current task, fixture setup | Match the dev provider's JSON-escaped quoted output; rerun produced the intended product RED |
| Required backend artifact is stale (`0.33.1`) | Current task, phase 3 preflight | Stop before requests; rebuilding is outside this turn's phases 3-5 scope |
| Resident requested a second `task` permission | Current task, resident slice | Leave unanswered and stop all live work per the one-reply safety boundary |
| Full Bun suite reported `Session is busy` twice | Current task, verification | Reproduced the pre-existing admission race; wait for the existing session status to leave `busy` before starting shell activity |
| Fresh resident direct wake received relay HTTP-parser 400 | Current task, fresh resident retry | Preserve canonical delivery/failure evidence; stop at the six-forward boundary and do not borrow nested allocation |
| Final retry readiness probe used absent `/health` route | Final mailbox setup, zero provider requests | Restart with the known `/session` readiness route |
| Legacy create rejected redundant `modelID` object | Final mailbox setup, zero provider requests | Use the server's exact pinned default model; retain explicit title/workdir |
| Raw text monitor matched nonce inside user prompt | Final mailbox retry | Reclassify from canonical role-correlated replay; issue no extra prompt or permission |
| Fresh attempt 13 returned local HTTP 429 | Final mailbox retry | Preserve partial direct-mail pass; stop at `27 / 27` aggregate and leave channel/final synthesis incomplete |
