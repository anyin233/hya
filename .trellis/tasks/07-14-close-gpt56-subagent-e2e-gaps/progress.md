# Progress

## 2026-07-14

- Created and activated the planning pointer for
  `.trellis/tasks/07-14-close-gpt56-subagent-e2e-gaps`.
- Read the prior E2E result/finding records, Trellis workflow, domain glossary,
  mailbox/resident/TUI ADRs, and relevant runtime/test call paths.
- Confirmed the task is diagnosis-first and that no live provider authorization
  carries over from the prior run.
- Seeded PRD, design, implementation plan, and persistent planning files.
- Ran two independent planners with conservative implementation and risk/failure
  framings. Merged their plans in the main session.
- Resolved effective TypeScript navigation (`Ctrl+X`, `Down`; `Up`), discovered
  the missing explicit read-only footer marker, and identified auto-title as an
  outbound request not represented by normal turn evidence.
- Chose a recommended hard cap of 20 outbound requests/30 minutes, resident and
  TUI before nested, and separate direct/channel resident wake cycles.
- Curated 7 implementation and 7 check context entries; Trellis validation and
  `git diff --check` passed. Planning is complete and execution awaits user
  approval of the merged plan and cap.
- User approved the merged plan and a hard limit of 20 outbound GPT 5.6 Sol
  requests and 30 minutes. `task.py start` changed the task to `in_progress`.
- Built current `hya-backend` and `hya-ts` binaries successfully with
  `cargo build -p hya-backend -p hya-ts --bins`.
- Added the focused PTY test `Linux PTY child observation is visibly read-only
  and preserves the root draft`. The first run exposed a fixture predicate bug
  caused by JSON-escaped quotes; after correcting the predicate, the test
  reached the intended product RED solely because no visible `read-only` text
  was rendered.
- Added `Read-only` to the existing `SubagentFooter`. The focused test then
  passed with 1 test, 11 assertions, and 0 failures. It also proves ordinary
  child-view text cannot submit a prompt and the root draft survives navigation.
- Updated release metadata to `0.33.2`: workspace and TypeScript package
  versions, a single-version root changelog, and archived `0.33.1` notes.
- `cargo metadata --no-deps --format-version 1` and feature-scoped
  `git diff --check` passed. No paid provider request has been issued in this
  task yet.
- Began the approved phase 3 preflight and snapshotted the unrelated dirty tree.
  `target/debug/hya-backend --version` reported `0.33.1`, while current workspace
  metadata reports `0.33.2`; this violates the required current-binary boundary.
- Stopped before relay/runtime setup because rebuilding would rerun phase 2,
  outside this turn's phases 3-5 authorization. Forwarded request count remains
  exactly zero; no session, backend, PTY, relay, watchdog, or private database
  was created.
- Rechecked user config/auth modes and hashes unchanged (`0600`;
  config `03247bf6ce350e2df4c9b4c96ccbba6cd87287ef7b2ad453b872292866308f7a`,
  auth hash matched its pre-run value and is omitted from publishable evidence).
  No token content was read or logged.
- Main agent rebuilt the candidate; re-preflight passed with `hya-backend 0.33.2`,
  successful `hya-ts --help`/Cargo metadata, exact disposable model listing, and
  relay count exactly zero before prompting.
- Created a private runtime/database, counting relay, watchdog, disposable config,
  and auth symlink. Pre-titled root `hysec_wcCBPl83oW4G7CXK7qLE` on exact route
  `12th-oai/gpt-5.6-sol` before its first prompt.
- Replied once to the expected root `task` permission. Canonical events then
  recorded resident child `hysec_3WKayaI5ZjNqPq9p5o7N`, stable handle
  `general-1`, running result, registration, busy activity, roster, and one-recipient
  direct mail.
- Stopped immediately when the resident child requested a second `task`
  permission. It was not approved. Four provider requests were forwarded; no
  channel cycle, TUI, or nested attempt was run, and no product file was edited.
- Replayed the root and child from the private SQLite store, recorded redacted
  event/session anchors, stopped backend/relay/watchdog, verified an empty
  task-owned process inventory and unchanged config/auth modes/hashes, then
  removed the private runtime directory.
- Verification passed for `cargo fmt --all --check`, workspace Clippy with
  warnings denied, `cargo test --workspace`, the `0.33.2` binary build,
  TypeScript typecheck, and TypeScript build.
- The first full `bun test` run had one `Session is busy` failure in the
  retained real-backend workflow; all 9 other tests, including both PTY tests,
  passed. Read-only diagnosis proved the test observes streamed text before the
  async prompt releases its run guard, then races an immediate shell request.
  The implicated sequence predates this task and uses an isolated backend/DB.
- Initial focused stress and one full-suite rerun passed without changes, but a
  later full-suite run reproduced the same race. That established a valid RED.
  Added one bounded poll using the test's existing session-status API before
  starting shell activity. The full 10-test suite, typecheck, build, and 50
  focused workflow reruns then passed.
- Strengthened the child-composer assertion with a positive root-view control;
  the first narrowly sliced assertion produced the expected RED, and the final
  full-frame assertion plus child absence check passed.
- Closed the remaining draft-isolation false-pass path by capturing the restored
  root frame and asserting the child sentinel is absent. The focused PTY test
  passed with 13 assertions; the final Bun suite passed all 10 tests.
- Final scoped review found no remaining code, test, release-metadata, evidence,
  or secret-hygiene issues. `git diff --check` and Trellis validation passed;
  no `hya-backend`/`hya-ts` process or private E2E/PTY directory remains.
- The Trellis task remains `in_progress`: the resident permission safety stop
  prevented live mailbox/TUI evidence, and nested was not run. No commit, push,
  archive, or task finish was performed.
- User authorized one fresh retry using the 16 unused requests under a new
  30-minute window: resident/live TUI may use 6 and nested may use 10. The task
  aggregate remains capped at 20 requests. Only the root resident-spawn `task`
  may be approved; any child `task` permission still stops the run.
- Fresh preflight passed with current `0.33.2` binaries, exact disposable model
  listing, original config/auth mode/hash, empty process inventory, and relay
  count zero before the pre-titled root prompt.
- Fresh root `hysec_d3FNiBjNs0buTHPKbVJR` spawned resident
  `hysec_Nif1stdPAZpN2OxeMx3G`; the exact root `task` permission was approved
  once. No child, second task, or unrelated permission appeared.
- Canonical events proved stable handle `fresh-resident-nonce-responder-1`,
  registration, initial busy/idle, one main synthesis wake, post-idle roster,
  and exact one-recipient direct delivery.
- A real current `hya-ts` PTY passed `Ctrl+X`/Down/Up observation, visible
  `Read-only`, absent child Prompt composer, ignored ordinary child text with
  unchanged events/request count, and exact root draft restoration.
- The direct wake failed at malformed private-relay request framing before its
  nonce reply. Fresh forwarded usage reached 6; attempts 7-8 were rejected
  locally. Aggregate usage is `4 + 6 = 10`; no channel or nested request was run.
- Replayed fresh root range `1-444` and child range `348-432`, stopped
  PTY/backend/relay/watchdog, verified unchanged config/auth hashes and modes and
  an empty process inventory, then removed only the private fresh runtime.
- User authorized the nested permission boundary: approve exactly one correlated
  root-to-depth-1 `task` and one correlated depth-1-to-depth-2 `task`. Any third
  `task` or unrelated permission stops immediately. Nested keeps the untouched
  10-request allocation and a fresh 30-minute window.
- Built a corrected private relay that fully dechunks incoming requests, strips
  all hop-by-hop/framing headers, and sends explicit upstream `Content-Length`.
  A private local sink received exactly one intact 9766-byte chunked test body
  with the expected SHA-256 and no `Transfer-Encoding`; provider count stayed 0.
- Preflight then passed current binary/model, private DB/config/auth symlink,
  pre-titled root, governor limits, zero relay count, fresh watchdog, and original
  hash/mode/process constraints.
- Nested attempt 1 passed using 5 forwarded requests. Root
  `hysec_4lN7lPZqFffzIaTcJrqZ`, child `hysec_6gy7W7zgkTGRNRbz59GP`, and grandchild
  `hysec_YLdVM6BFVXl9Sc6uJS6s` canonically proved exact route, both ancestry
  edges, both task calls/results, admission/lifecycle/completion, and nonces
  `NROOT_7F3`, `NCHILD_8G4`, and `NGRAND_9H5` propagated to root.
- Replied once to exactly the authorized root and depth-1 `task` permissions; no
  third/wrong-session/unrelated permission appeared. Aggregate provider usage is
  now `4 + 6 + 5 = 15 / 20`.
- Replayed all three sessions from private SQLite, stopped backend/relay/local
  sink/watchdog, verified unchanged config/auth modes and hashes and empty process
  inventory, then removed only the private nested runtime.
- User authorized one final corrected-relay mailbox-only retry with up to 12
  additional requests and a fresh 30-minute window, explicitly raising the
  aggregate ceiling from 20 to 27. No live TUI or nested rerun is authorized;
  only the root resident-spawn `task` permission may be approved.
- Reproved corrected relay framing at zero provider count: one private sink hit,
  intact 9766-byte chunked body, matching SHA-256, explicit content length, and
  no transfer encoding. Disposable exact-route listing and auth symlink passed.
- Corrected two zero-provider driver setup errors before live work: readiness uses
  `/session`, not absent `/health`, and the redundant invalid legacy create model
  object was removed in favor of the backend's exact pinned default.
- Final mailbox root `hysec_S7rDpH7mg9rrR0jZpLY6` and resident
  `hysec_orFRFrYT6XxW1rbE2A72` proved exact route, stable handle, both channel
  joins, exact two-member channel listing, initial idle, one-recipient direct
  delivery, nonce reply, and return to idle.
- Replied `once` to exactly the root resident-spawn permission. No child, second,
  wrong-session, or unrelated permission appeared; the pending queue was empty.
- The monitor's raw text predicate initially matched the completion nonce in the
  user prompt. Role-correlated canonical replay corrected the result: no assistant
  completion contained the nonce, and no extra permission or prompt was issued.
- All 12 fresh requests returned HTTP 200. Fresh attempt 13 was counted and
  rejected locally with HTTP 429. Main had correctly withheld channel mail while
  roster still showed the resident busy; the cap then blocked the next quiescence
  turn, so channel send/leave and completed final synthesis remain incomplete.
- Aggregate forwarded usage is `4 + 6 + 5 + 12 = 27 / 27`. Offline CLI replay
  exactly matched 109 root and 68 child HTTP envelopes and SQLite payload ranges.
- Stopped backend/relay/sink/watchdog, verified unchanged original config/auth
  hashes and modes and an empty process inventory. Task remains `in_progress` for
  main-agent review; no additional live provider request is authorized.
- Final scoped review found no code, test, secret, accounting, release-metadata,
  or task-status defect. Corrected only two stale evidence labels: the first-run
  classification is now explicitly historical, and the superseded relay blocker
  is no longer listed as current. The task remains `in_progress` solely because
  channel send/leave and completed final synthesis are unmet.
