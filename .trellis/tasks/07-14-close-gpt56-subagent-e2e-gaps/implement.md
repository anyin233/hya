# Implementation Plan

## 1. Finish Planning

- [x] Merge at least two independent diagnosis plans into these artifacts.
- [x] Resolve current TUI observation bindings and existing focused test seams
      from source/tests.
- [x] Record the parallel-plan merge, including disagreements and resolutions.
- [x] Curate real `implement.jsonl` and `check.jsonl` entries.
- [x] Validate the task artifacts.
- [x] Record user approval of the plan and a fresh paid request/time cap.
- [x] Start the Trellis task only after review.

## 2. Run Zero-Cost TUI Diagnosis

- [x] Build current `hya-backend` and `hya-ts` source.
- [x] Start the current backend on the dev provider with a private database and
      create a titled parent plus child fixture.
- [x] Place a nonce draft in the main composer, navigate with effective
      `Ctrl+X`, then `Down`, and assert child transcript/status.
- [x] Assert a visible read-only marker and no Prompt composer in the child.
- [x] Type a distinct child sentinel, return with `Up`, and assert no prompt
      request/event plus an unchanged main draft.
- [x] Classify any failure as driver timing, current-source product behavior, or
      installed-version drift before entering a fix branch.

## 3. Preflight The Bounded Live Run

- [x] Snapshot relevant workspace status without touching unrelated files.
- [x] Verify installed `hya-backend`, `hya-ts`, exact model listing, governor
      environment, auth/config modes, and auth hash.
- [x] Create a private runtime directory/database and a cleanup trap/process
      inventory.
- [x] Start a private localhost relay that increments before forwarding, logs no
      headers or bodies, rejects request 21, and starts a 30-minute watchdog on
      the first request.
- [x] Use a disposable provider config that points to the relay while preserving
      the existing auth secret and original config/auth hashes and modes.
- [x] Pre-title each root session and record zero initial provider requests.

The main agent rebuilt the workspace candidate. Re-preflight confirmed
`hya-backend 0.33.2`, the exact route, zero initial relay requests, private
runtime/config/auth-reference boundaries, and the required governor values.

## 4. Run Resident, Mailbox, And TUI Slice

- [x] Start the installed backend and real PTY on
      `12th-oai/gpt-5.6-sol` without `--yolo`.
- [x] Spawn one resident and assert immediate running outcome plus stable handle.
- [x] Assert registration and initial idle state from the team-root projection.
- [x] Exercise roster, join, and channels, then send direct nonce mail.
- [x] Assert the direct-mail resident turn/reply and return to idle before
      sending channel nonce mail.
- [ ] Assert a second channel-mail turn/reply and return to idle, then leave.
- [ ] Assert exact event delivery, membership, and recipient counts.
- [ ] Assert quiescence causes one main synthesis wake and no budget breach.
- [x] Open the current child observation with `Ctrl+X`, then `Down`.
- [x] Assert child transcript/status/read-only marker and absent Prompt composer.
- [x] Inject ordinary text, assert no main prompt mutation/submission, return to
      main with `Up`, and assert normal composer availability.

## 5. Run Nested Slice

- [x] Submit one nonce-bearing nested instruction after resident/TUI evidence is
      preserved.
- [x] Resolve root, depth-1, and depth-2 session IDs from canonical events.
- [x] Assert exact route, ancestry, nested tool call, lifecycle, completion, and
      correlated nonce result.
- [x] Permit at most two attempts and 10 forwarded requests for nested.
- [x] On a pre-`StepStarted` provider 5xx, classify the attempt external; after
      the second such failure, close only this slice as externally blocked.

Resident spawn reached a stable running handle, registration, and a direct send,
but the resident was still busy and requested a second `task` permission. The
single allowed root permission had already been replied once. The second request
was left unanswered and all live work stopped; live TUI and nested were not run.

Fresh resident/TUI retry: root `hysec_d3FNiBjNs0buTHPKbVJR` and resident
`hysec_Nif1stdPAZpN2OxeMx3G` proved exact route, stable registration, initial
idle, roster, one-recipient direct delivery, and all live TUI assertions. The
resident direct wake failed at malformed private-relay request framing before a
nonce reply. Six fresh requests were forwarded; attempts 7-8 were rejected
locally, so channel work was not attempted and nested remains untouched.

Nested passed on attempt 1 after a zero-provider local framing preflight. Root
`hysec_4lN7lPZqFffzIaTcJrqZ`, child `hysec_6gy7W7zgkTGRNRbz59GP`, and grandchild
`hysec_YLdVM6BFVXl9Sc6uJS6s` proved exact route, depth/ancestry, two correlated
task calls/results, complete lifecycle, and root/child/grandchild nonce
propagation with 5 forwarded nested requests.

Final mailbox retry used all 12 authorized forwards. Root
`hysec_S7rDpH7mg9rrR0jZpLY6` and resident `hysec_orFRFrYT6XxW1rbE2A72` proved
stable registration, both channel joins, exact two-member channels, initial and
direct-mail idle cycles, one-recipient direct delivery, and nonce reply. Main
correctly withheld channel mail while roster still showed the resident busy;
after it became idle, fresh attempt 13 rejected the final quiescence turn locally.
Channel send/leave and completed final synthesis therefore remain incomplete.

## 6. Diagnose Only Reproducible Local Failures

- [x] Reproduce the exact symptom against current source with one fast,
      deterministic, agent-runnable command.
- [x] Record 3-5 ranked falsifiable hypotheses before instrumentation.
- [x] Add the smallest behavior-contract regression test and observe expected
      RED.
- [x] Apply the minimum root-cause fix and observe focused GREEN.
- [x] Rerun the original live slice within the remaining approved cap.
- [x] If product source changed, update `[workspace.package].version`, archive
      the previous root changelog, and write the new single-version changelog.

## 7. Verify And Close

- [x] For Rust changes run `cargo fmt --all --check`.
- [x] For Rust changes run
      `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] For Rust changes run `cargo test --workspace` and build a local executable.
- [x] For TypeScript TUI changes run its existing typecheck, test, and build
      commands discovered from the package manifest.
- [x] Re-run every affected focused slice and replay canonical events offline.
- [x] Write final E2E evidence with request count, event/session anchors,
      classifications, and no secrets.
- [x] Stop all processes including relay/watchdog, remove only private runtime
      artifacts, verify config and auth metadata/hash, inspect diff/status, and
      validate the task.
- [x] Run Trellis quality/spec review. Commit or push only with explicit user
      authorization, then finish/archive according to the active workflow.
