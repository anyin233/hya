# Bounded Live E2E Results

## Final Aggregate Classification

- **Live TUI passed** on current `0.33.2`: child transcript/status/model and
  visible `Read-only`, no child Prompt composer, ignored ordinary input, and an
  exactly restored root draft.
- **Nested passed** at depth 2 on attempt 1 with exact ancestry, route, lifecycle,
  tool correlation, and root/child/grandchild nonce propagation.
- **Resident mailbox partially passed**: registration, stable handle, channel
  membership/listing, direct delivery, direct nonce reply, and return to idle are
  canonical passes. The authorized cap was reached before channel send/leave and
  completed final synthesis.
- Aggregate provider usage was **27 / 27 forwarded requests**. No further live
  request is authorized, and no local product defect is established by the two
  remaining bounded gaps.

## Initial Attempt Classification

- **Preflight passed.** `target/debug/hya-backend --version` reported `0.33.2`;
  `target/debug/hya-ts --help`, `cargo metadata --no-deps --format-version 1`,
  and the disposable exact-route model listing passed.
- **Resident/mailbox/TUI stopped by the permission safety boundary.** The root
  correctly spawned one resident and approved its one expected `task` permission
  once. The resident then requested a second `task` permission. It was not
  approved; all live work stopped immediately.
- This is incomplete model/automation adherence, not a reproduced local product
  defect. The root also sent direct mail while the resident was still canonically
  `busy`, so the required initial-idle/direct-wake sequence was not established.
- **Nested was not run** because the prior slice's hard safety stop terminates the
  run. No additional product source, version, or changelog change was made in
  response to the live stop.

## Request And Runtime Boundary

- Exact route: `12th-oai/gpt-5.6-sol` for root and child.
- Forwarded provider requests: **4 / 20**, all in the resident allocation and all
  returned HTTP 200.
- Resident/mailbox/TUI allocation: **4 / 10**; classification: **safety stop,
  incomplete**.
- Nested allocation: **0 / 10**; attempts: **0 / 2**; classification: **not run
  after safety stop**.
- The 30-minute deadline began at forwarded request 1 and was not reached.
- Permission observation: root `task` permission was replied `once`; child
  `task` permission was observed and deliberately left unanswered. No unrelated
  permission action appeared.

### Fresh Retry Authorization

- Prior usage remains **4 / 20**.
- The user authorized at most **16 additional requests** under a fresh
  30-minute window: **6** for resident/live TUI and **10** for nested.
- This authorization does not increase the aggregate 20-request ceiling.
- Only the new root resident-spawn `task` may be approved. Any child `task` or
  unrelated permission remains an immediate stop.

### Nested Permission Authorization

- Nested starts from aggregate usage **10 / 20** and may use at most **10**
  additional requests under a fresh 30-minute window.
- Exactly two correlated `task` permissions may be approved: the root spawning
  depth 1 and the depth-1 child spawning depth 2.
- Any third `task` or unrelated permission remains an immediate unanswered stop.

### Final Mailbox Retry Authorization

- After nested, aggregate usage is **15** requests.
- The user explicitly authorized up to **12 additional requests** and a fresh
  30-minute window for one corrected-relay mailbox-only retry, raising the
  aggregate ceiling to **27**.
- No TUI or nested rerun is authorized. Only the root resident-spawn `task` may
  be approved; any child `task` or unrelated permission is an immediate stop.

## Canonical Anchors

- Root: `hysec_wcCBPl83oW4G7CXK7qLE`, exact model
  `12th-oai/gpt-5.6-sol`, titled before prompting; SQLite event range `1-293`.
- Resident depth-1 child: `hysec_3WKayaI5ZjNqPq9p5o7N`, parent root above,
  exact model `12th-oai/gpt-5.6-sol`; SQLite event range `147-291`.
- No grandchild was admitted.
- Root event `148`: resident `MemberSpawned`, depth 1.
- Root event `149`: `AgentRegistered`, handle `general-1`, mode `resident`.
- Root event `150`: correlated nonblocking task result, stable child ID, status
  `running`, handle `general-1`.
- Root event `151`: resident activity changed to `busy`.
- Root events `164/167`: roster call/result showed `general-1` resident and busy.
- Root event `168`: one direct `MailSent` from `main` to `general-1`; event `169`
  records exactly one recipient.
- Child event `159`: `StepStarted`; child event `290`: unexpected nested `task`
  call request; child event `291`: step finished for tool calls. The matching
  second permission remained pending at shutdown.
- No channel join/list/send/leave, direct reply, idle return, quiescence
  synthesis, live TUI frame, child-input assertion, or nested result is claimed.

## Relay Boundary

The private relay incremented and atomically persisted each ordinal before
forwarding, rejected requests above the total/slice/deadline limits, and logged
only ordinal, monotonic timestamp, path, and status. Final log:

```text
1 /v1/chat/completions 200
2 /v1/chat/completions 200
3 /v1/chat/completions 200
4 /v1/chat/completions 200
```

No request headers, bodies, or tokens were logged.

## Security And Cleanup

- Original user config: mode `0600`, SHA-256
  `03247bf6ce350e2df4c9b4c96ccbba6cd87287ef7b2ad453b872292866308f7a`.
- Original `12th-oai` auth: mode `0600`; its hash matched the pre-run value and
  is omitted from publishable evidence.
- The disposable auth path was a symlink to the original file. The token was not
  read, printed, copied, persisted, or logged.
- Backend, relay, and watchdog were stopped immediately at the safety boundary.
- Final config/auth modes remained `0600` and both hashes matched their pre-run
  values. Process inventory was empty, and the private runtime directory was
  removed after canonical anchors were redacted into this file.

## Fresh Resident/TUI Retry

### Classification And Accounting

- Fresh classification: **partial pass; relay/automation blocked at the direct
  wake, then the fresh request boundary stopped further work**.
- Fresh forwarded requests: **6 / 6**. Fresh attempts 7 and 8 were rejected
  locally with HTTP 429; neither was forwarded.
- Aggregate usage: **4 prior + 6 fresh = 10 / 20**. The nested allocation was
  not used or borrowed from and remains outside this retry.
- All six forwarded ordinals logged HTTP 200. The resident direct wake then
  received an HTML HTTP-parser 400 showing malformed request framing at the
  private relay boundary. This is relay/automation evidence, not a reproduced
  product or provider defect.
- The fresh 30-minute window started on request 1 and was not reached.

### Sessions And Canonical Events

- Fresh root: `hysec_d3FNiBjNs0buTHPKbVJR`, exact route
  `12th-oai/gpt-5.6-sol`, explicitly titled before prompting; SQLite event range
  `1-444`.
- Fresh resident: `hysec_Nif1stdPAZpN2OxeMx3G`, parent fresh root, exact route
  `12th-oai/gpt-5.6-sol`; SQLite event range `348-432`.
- Root event `345`: the only `task` tool request. The matching exact root
  resident-spawn permission was replied `once`.
- Root events `349-351`: depth-1 resident spawn, registration as stable handle
  `fresh-resident-nonce-responder-1`, and correlated nonblocking running result.
- Root events `352/366`: initial resident `busy -> idle`. Direct mail was not
  admitted before this idle event.
- Root events `367-382`: one initial main synthesis wake and return to idle.
- Root events `392/394`: roster call/result after initial idle.
- Root events `420/422/423`: direct send call, one `MailSent` to the stable
  handle, and delivery result with exactly one recipient.
- Root events `424/433/434`: direct wake became busy, failed at the relay framing
  boundary, then returned idle without the required nonce reply.
- Root events `435/443/444`: a subsequent main synthesis attempt was rejected at
  the fresh relay cap and returned idle. Therefore the required final one-shot
  quiescence synthesis criterion is not claimed.
- No channel join/list/send/leave, membership count, channel reply, or nested
  spawn occurred. No governor turn/message budget breach was observed.

### Permission Observation

- Replied exactly once to the root's `task` permission for call
  `tc_019f62ad733a7313b47d4e974499eaea`.
- No child `task`, second resident-slice `task`, or unrelated permission appeared.
- Pending permission list was empty at shutdown.

### Live TUI Assertions

- A real current `target/debug/hya-ts` PTY was attached to the fresh live root and
  resident through the live backend.
- `Ctrl+X`, then `Down` opened the resident view. The sanitized frame contained
  the resident transcript nonce `RESIDENT_READY_R56_FRESH`, status/model text,
  and visible `Subagent Read-only (1 of 1)`; it contained no `Prompt` composer.
- Ordinary text `CHILD_SENTINEL_R56_FRESH_IGNORED` caused no root or child event
  count change, no permission, no provider request, and did not appear in the
  child frame.
- `Up` restored the root composer and exact draft `ROOT_DRAFT_R56_FRESH_KEEP`;
  the child sentinel was absent from the restored root draft.

### Fresh Relay Log

```text
1 /v1/chat/completions 200
2 /v1/chat/completions 200
3 /v1/chat/completions 200
4 /v1/chat/completions 200
5 /v1/chat/completions 200
6 /v1/chat/completions 200
7 /v1/chat/completions 429 (local rejection)
8 /v1/chat/completions 429 (local rejection)
```

The relay logged only ordinal, monotonic timestamp, path, and status; it logged
no headers, bodies, or tokens.

### Fresh Cleanup

- The PTY/tmux server, backend, relay, and watchdog were stopped.
- Original config/auth modes remained `0600`; config SHA-256 remained
  `03247bf6ce350e2df4c9b4c96ccbba6cd87287ef7b2ad453b872292866308f7a`, and the
  auth hash matched its pre-run value but is omitted from publishable evidence.
- The auth token was never read, printed, copied, persisted, or logged. The
  disposable auth entry was only a symlink to the original file.
- Final process inventory was empty; the private fresh runtime was removed after
  replay and redaction.

## Nested Slice

### Relay Framing Preflight

- Before any provider request, a private local sink received one chunked POST
  through the corrected relay.
- The relay fully dechunked the request, removed hop-by-hop/framing headers, and
  forwarded one intact `9766`-byte body with explicit `Content-Length: 9766` and
  no `Transfer-Encoding`.
- Sink hit count was exactly one. Received SHA-256 matched expected SHA-256:
  `939dc7422c723ea64ab32692c5c1089c81b935544dda326c029143505700cd5a`.
- The local framing check did not contact the provider and left nested relay
  state exactly `attempted=0`, `forwarded=0`, `first=null`.

### Classification And Accounting

- Nested classification: **pass on attempt 1**. No tighter second attempt was
  needed; no provider 5xx occurred.
- Nested forwarded requests: **5 / 10**; all five returned HTTP 200. No request
  was rejected, and the fresh 30-minute window was not reached.
- Aggregate usage: **4 prior + 6 resident retry + 5 nested = 15 / 20**.
- No resident/mailbox or live-TUI action was rerun.

### Sessions And Canonical Events

- Root `hysec_4lN7lPZqFffzIaTcJrqZ`, pre-titled before prompting, exact route
  `12th-oai/gpt-5.6-sol`; SQLite event range `1-275`.
- Depth-1 child `hysec_6gy7W7zgkTGRNRbz59GP`, parent root, exact route
  `12th-oai/gpt-5.6-sol`; SQLite event range `163-262`.
- Depth-2 grandchild `hysec_YLdVM6BFVXl9Sc6uJS6s`, parent depth-1 child, exact
  route `12th-oai/gpt-5.6-sol`; SQLite event range `234-249`.
- Root event `160`: first `task` request, call
  `019f62bc-55c7-7613-8694-68d78abdce8b`; root events `164/166` prove depth-1
  admission and running lifecycle.
- Child event `232`: nested `task` request, call
  `019f62bc-830e-7500-8e0f-7ae993a65564`; child events `235/237` prove depth-2
  admission and running lifecycle.
- Grandchild events `244/248/249` prove `StepStarted`, successful stop, and
  assistant completion containing nonce `NGRAND_9H5`.
- Child event `250` records depth-2 `done` with summary `NGRAND_9H5`; child event
  `256` is the tool result correlated to call
  `019f62bc-830e-7500-8e0f-7ae993a65564`; child completion contains
  `NCHILD_8G4 PROPAGATED NGRAND_9H5`.
- Root event `263` records depth-1 `done` with the propagated summary; root event
  `269` is the tool result correlated to call
  `019f62bc-55c7-7613-8694-68d78abdce8b`; root event `275` completes with
  `NROOT_7F3 CORRELATED NCHILD_8G4 PROPAGATED NGRAND_9H5`.
- Session listing and SQLite replay agreed on both ancestry edges, exact route,
  event ranges, lifecycle, tool correlation, and nonce propagation.

### Permission Observation

- Replied `once` to exactly two correlated permissions:
  1. root `hysec_4lN7lPZqFffzIaTcJrqZ` for root task call
     `tc_019f62bc55c77613869468d78abdce8b`;
  2. depth-1 child `hysec_6gy7W7zgkTGRNRbz59GP` for nested task call
     `tc_019f62bc830e75008e0f7ae993a65564`.
- No third `task`, wrong-session `task`, or unrelated permission appeared.
- Pending permission queue was empty at shutdown.

### Nested Relay Log

```text
1 /v1/chat/completions 200
2 /v1/chat/completions 200
3 /v1/chat/completions 200
4 /v1/chat/completions 200
5 /v1/chat/completions 200
```

The relay atomically incremented before forwarding and logged only ordinal,
monotonic timestamp, path, and status. It logged no headers, bodies, or tokens.

### Nested Cleanup

- Backend, relay, local preflight sink, and watchdog were stopped. No PTY helper
  was started for this slice.
- Original config/auth modes remained `0600`; config SHA-256 remained
  `03247bf6ce350e2df4c9b4c96ccbba6cd87287ef7b2ad453b872292866308f7a`, and the
  auth hash matched its pre-run value but is omitted from publishable evidence.
- Auth was referenced only through the disposable symlink. No secret was read,
  printed, copied, persisted, or logged.
- Final nested process inventory was empty; the private runtime was removed after
  canonical replay and redaction.

## Verification

- Passed: `cargo fmt --all --check`.
- Passed: `cargo clippy --workspace --all-targets -- -D warnings`.
- Passed: `cargo test --workspace`.
- Passed: `cargo build -p hya-backend -p hya-ts --bins`; the backend reports
  `0.33.2`.
- Passed: TypeScript `bun run typecheck` and `bun run build`.
- TypeScript `bun test` reproduced a pre-existing `Session is busy` race twice
  in `real-backend.test.ts`: streamed text can precede release of the session
  run guard. The test now waits on the existing status API before starting its
  shell turn. The full suite, both PTY tests, and 50 focused workflow reruns pass.
- The focused child-observation PTY test passes with 13 assertions, including a
  positive root composer control and proof that the child sentinel is absent
  from the restored root draft.
- Final read-only scoped review found no remaining findings. Task validation and
  scoped whitespace checks pass; no task-owned process or private runtime remains.

## Final Mailbox Retry

### Relay Framing Preflight

- Before provider traffic, a private sink received exactly one chunked POST
  through the corrected relay. The intact `9766`-byte body had SHA-256
  `7b50ffe613cfe1154892cf2a6483dd891764b2842e75d6fb2c2879ce5cd87687`.
- The forwarded request carried explicit `Content-Length: 9766` and no
  `Transfer-Encoding`; sink hit count was one and provider state remained exactly
  `attempted=0`, `forwarded=0`, `first=null`.

### Classification And Accounting

- Final mailbox classification: **partial pass at the authorized hard cap**.
  Registration, initial idle, roster, both channel joins, channel listing, direct
  delivery, direct nonce reply, and return to idle passed. Channel mail, leave,
  and a completed final synthesis did not run.
- Fresh forwarded requests: **12 / 12**, all HTTP 200. Fresh attempt 13 was
  atomically counted and rejected locally with HTTP 429 before forwarding.
- Aggregate usage: **4 prior + 6 resident retry + 5 nested + 12 final mailbox =
  27 / 27 forwarded**. The fresh 30-minute window was not reached.
- No tighter prompt was issued because the full authorization was consumed. No
  TUI, nested, or previously passing slice was rerun.

### Sessions And Canonical Events

- Root `hysec_S7rDpH7mg9rrR0jZpLY6`, explicitly titled before prompting, exact
  route `12th-oai/gpt-5.6-sol`; SQLite event range `1-177` with 109 events.
- Resident `hysec_orFRFrYT6XxW1rbE2A72`, parent root, exact route
  `12th-oai/gpt-5.6-sol`; SQLite event range `19-162` with 68 events.
- Root event `15` is the only `task` request. Events `20-22` prove depth-1
  resident spawn, stable handle `mailbox-final-responder-1`, registration, and
  the correlated non-blocking `running` result.
- Root events `23/64` prove the initial resident `busy -> idle` cycle. Root event
  `25` joined main to `#mailbox-r56-final`; root event `50` records the resident
  join. Child event `57` and root event `81` both list exactly the two members,
  `main` and `mailbox-final-responder-1`.
- Root events `77/80` prove roster observed the resident idle before direct mail.
  Root events `106/108/109` prove direct nonce `DIRECT_R56_FINAL_4K2`, one
  `MailSent`, and exactly one recipient.
- Root events `110/163` prove the separate direct-mail resident `busy -> idle`
  cycle. Child events `140/143` and root event `142` prove the resident sent
  `DIRECT_ACK_R56_FINAL_4K2` back to main with exactly one recipient.
- Root events `154/156` show main correctly rechecked roster after the direct ACK
  and observed the resident still busy, so it did not prematurely send channel
  mail. The resident returned idle at event `163`.
- Root event `168` is the next and only final quiescence synthesis wake. Fresh
  attempt 13 was rejected locally; root events `175-177` record the failed turn
  and return to idle. No channel nonce `MailSent`, `ChannelLeft`, completed final
  synthesis, repeated synthesis, child `task`, or governor budget breach exists.
- Live `/sessions/:id/events`, Compat session listing, offline `tail-session`, and
  SQLite `event_log` payload ranges agreed. Offline replay exactly matched all
  109 root and 68 child envelopes.

### Permission And Automation Boundary

- Replied `once` to exactly root permission
  `perm_019f62d1fe6e75b0ad1c22438b1af200`, correlated to root task call
  `tc_019f62d1fe677b53a05ac6ff10ba6c01`.
- No child, second, wrong-session, or unrelated permission appeared. The pending
  permission queue was empty at shutdown.
- Two zero-provider setup corrections were required: `/health` is not a server
  route, and legacy session creation expects model key `id` rather than
  `modelID`. The corrected readiness route and backend default model were used.
- The monitor initially mistook `FINAL_SYNTH_R56_FINAL` inside the user prompt's
  raw `text_delta` for assistant completion. Canonical role-correlated replay
  corrected the classification; no assistant completion contained that nonce.

### Final Mailbox Relay Log

```text
1 /v1/chat/completions 200
2 /v1/chat/completions 200
3 /v1/chat/completions 200
4 /v1/chat/completions 200
5 /v1/chat/completions 200
6 /v1/chat/completions 200
7 /v1/chat/completions 200
8 /v1/chat/completions 200
9 /v1/chat/completions 200
10 /v1/chat/completions 200
11 /v1/chat/completions 200
12 /v1/chat/completions 200
13 /v1/chat/completions 429 (local rejection)
```

The relay atomically counted before forwarding and logged only ordinal,
monotonic timestamp, path, and status. Concurrent responses caused ordinals 10
and 11 to be written in completion order; no headers, bodies, or tokens were
logged.

### Final Mailbox Cleanup

- Backend, relay, private sink, and watchdog were stopped. No TUI or PTY helper
  was started.
- Original config/auth modes remained `0600`; config SHA-256 remained
  `03247bf6ce350e2df4c9b4c96ccbba6cd87287ef7b2ad453b872292866308f7a`, and the
  auth hash matched its pre-run value but is omitted from publishable evidence.
- Auth was referenced only through the disposable symlink. No secret was read,
  printed, copied, persisted, or logged. Final process inventory was empty.
