# Findings

- Prior canonical evidence already covers discovery, foreground, resume,
  parallel category/inline, and background execution; rerunning them would spend
  provider calls without closing a gap.
- The nested HTTP 524 occurred after child admission and prompt recording but
  before `StepStarted`, nested tool calls, or permissions. No local governor or
  spawn defect is established.
- `spawn_team_supervisor` separates resident and transient members. Pure
  resident spawn replies immediately and `ResidentSupervisor` owns wake and
  quiescence behavior.
- Mailbox operations append `AgentRegistered`, `MailSent`, `ChannelJoined`, and
  `ChannelLeft` to the team-root log and read roster/channel state from the
  projection.
- Existing resident tests cover one-turn wake, quiescence synthesis, and message
  budget cancellation; these are the first local seams if live behavior fails.
- ADR-0003 requires observation views to ignore ordinary text and omit the
  Prompt composer. Current TypeScript bindings are `Ctrl+X`, then `Down` to the
  first child and `Up` to the parent. A failed chord alone is not defect
  evidence; current bindings and PTY timing must be used.
- The focused real-PTY test confirmed the current `SubagentFooter` lacked the
  explicit read-only marker while child navigation, input isolation, and root
  draft preservation already worked. Rendering `Read-only` in that footer is
  sufficient; no new state or input handling is required.
- Dev-provider transcript output containing quotes is JSON-escaped in the PTY
  fixture. Readiness predicates must match the escaped representation or they
  time out before reaching the behavior under test.
- Compat prompt admission starts auto-title in the background; canonical turn
  events therefore undercount outbound provider calls. A hard cap must increment
  before forwarding at the HTTP boundary, and roots should be pre-titled.
- Independent planners recommended 22 requests/20 minutes and 20 requests/30
  minutes. The merge chose 20/30: it keeps a lower hard spend ceiling while
  allowing for the prior 130-second upstream timeout and two bounded nested
  attempts.
- The merge retained separate direct-mail and channel-mail wake/reply evidence
  from the original acceptance contract. Waiting for idle between sends prevents
  coalescing and makes each cycle independently verifiable.
- The working tree contains many pre-existing modified/untracked paths. This
  task must stage or edit only its own explicitly identified files.
- The current Trellis backend/frontend spec indexes are placeholders; ADRs,
  domain vocabulary, prior E2E evidence, and focused tests are more authoritative
  task context.
- The current workspace metadata is `0.33.2`, but the required
  `target/debug/hya-backend` artifact identifies itself as `0.33.1`. A live run
  with that artifact would violate the explicit current-built-binary acceptance
  boundary, so this is a pre-request blocker rather than resident, TUI, nested,
  provider, or product behavior evidence.
- The main-agent rebuild replaced the stale artifact; `hya-backend 0.33.2` and
  the disposable exact-route model listing passed with relay count zero.
- Canonical resident evidence reached a stable `general-1` running handle and
  registration, but activity was `busy` when the root issued direct mail. The
  child then requested another `task` call instead of executing its mailbox
  instructions. Because the only permitted approval had already been used for
  the root spawn, this is a hard safety stop and incomplete model adherence, not
  evidence of a mailbox, resident, TUI, nested, or provider defect.
- `real-backend.test.ts` waited for assistant text after admission-only
  `promptAsync`, but streamed text can become visible before the backend drops
  the session run guard. An immediate shell request therefore intermittently
  received the correct `Session is busy` response. The sequence predates this
  task and is unrelated to the footer, but it reproduced twice during the
  required gate. Polling the existing status endpoint until the session is no
  longer busy is the smallest deterministic harness fix.
- The fresh tighter resident prompt eliminated the prior child-spawn behavior:
  only the root `task` permission appeared, and registration plus initial idle
  completed canonically before direct mail.
- Current live `hya-ts` proved the full observation contract on the fresh root:
  transcript/status/model/read-only marker, absent child composer, ignored child
  text with unchanged events/requests, and restored exact root draft.
- Fresh mailbox evidence reached roster and exact one-recipient direct delivery.
  The following child provider request failed with an HTML HTTP-parser 400 caused
  by malformed private-relay framing despite the forwarded ordinal logging 200.
  The relay then rejected attempts 7-8 as required. This is an E2E relay/driver
  limitation, not evidence for a product or upstream provider source change.
- A relay must dechunk before forwarding and set a fresh explicit
  `Content-Length`; stripping `Transfer-Encoding` without dechunking reproduces
  malformed upstream framing under concurrent provider traffic. The corrected
  relay delivered one intact 9766-byte chunked test body to a private sink with
  no provider request.
- Nested passed on the first corrected-relay attempt. Canonical root/child/grandchild
  events proved depth 1/2 ancestry, exact model route, both task-call/result
  correlations, successful member lifecycle, and end-to-end nonce propagation.
  The prior pre-`StepStarted` 524 remains evidence for only that prior attempt.
- The final mailbox retry proved the corrected relay under the live resident path:
  12 concurrent/serial forwards returned 200 and fresh attempt 13 was rejected
  locally. Registration, join/channels, direct send/reply, and both resident idle
  boundaries are product passes; no framing failure recurred.
- The root obeyed the requested synchronization guard: after receiving the direct
  ACK it queried roster, saw the resident still busy, and did not send channel
  mail. The resident became idle shortly afterward, but the next quiescence turn
  reached the authorized cap. The remaining channel/leave gap is therefore a
  bounded-run/model sequencing gap, not a reproduced mailbox defect.
- Raw `text_delta` events do not carry message role. An E2E nonce predicate must
  correlate each message ID with canonical message role before treating text as
  assistant completion; otherwise a nonce embedded in the user instruction is a
  false positive. Offline replay corrected the final classification.
