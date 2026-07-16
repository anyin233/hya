# Implementation Plan

## 1. Finish Planning

- [x] Trace launcher, endpoint, parser, retry, roster, and production spawn flow.
- [x] Prove the real empty-root endpoint/parser and installed PTY roster path.
- [x] Merge all four required planner proposals and resolve disagreements.
- [x] Write `design.md`, `implement.md`, and sanitized research evidence.
- [x] Curate real `implement.jsonl` and `check.jsonl` entries.
- [x] Validate the task artifacts.
- [x] Obtain explicit approval for implementation and one live
      `12th-oai/gpt-5.6-sol` run capped at one root, two children, and ten
      minutes.
- [x] Run `task.py start` only after approval.

## 2. Run Zero-Cost Preflight

- [x] Snapshot installed versions, relevant binary paths, current model/agent
      listing, and repository dirty status without reading credentials.
- [x] Run `cargo test -p hya-app --test nested_spawn_tree`.
- [x] From `packages/hya-tui-ts`, run the focused workspace/PTY tests,
      `bun run typecheck`, and `bun run build`.
- [x] Build local `hya`, `hya-backend`, and `hya-ts` executables.
- [x] Create a private temporary run directory, SQLite database, process cleanup
      trap, and ten-minute watchdog.

Stop before provider traffic if any baseline contract fails. Diagnose that
failure independently instead of spending the approved live run.

## 3. Run One Live Root

- [x] Start installed `hya-backend` on loopback with the explicit database,
      model, and subagent governor values from `design.md`; do not use `--yolo`.
- [x] Attach installed `hya --server <url>` under the proven xterm transcript
      driver and submit one prompt requiring one foreground `task` call with
      exactly two `build` members.
- [x] Assign `RUST-RUNTIME` to summarize the event, projection, and server tree
      flow, and `TS-ROSTER` to summarize the TypeScript loader, parser, roster,
      observation, and focused tests.
- [x] Require read/glob/grep only, no nested delegation, and a root synthesis
      containing both exact labels.
- [x] On `0.33.7`, approve once only the two expected task-member permissions.
      On current `0.33.8`, accept the default policy's zero-prompt task path;
      if configured policy asks, validate and approve only those same two.
      Stop on every other permission, a third child, mutation attempt, provider
      failure, or timeout.

## 4. Verify The Child-Bearing Boundary

- [x] Identify the sole root from the fresh backend and poll its tree until two
      children are admitted or the run reaches a terminal failure.
- [x] Require HTTP success, the same top root from root and child routes, exactly
      two child session IDs, member type/status, and matching available roster
      metadata.
- [x] Feed the response to production `parseRunTree`; do not use an ad hoc shape
      parser as the acceptance check.
- [x] Attach installed `hya-ts` to the same root, open `Ctrl+X O`, require both
      children and no retry error, then open one child and require its focused
      read-only state.
- [x] Assert no prompt was admitted from the child observation and the root
      synthesis names `RUST-RUNTIME` and `TS-ROSTER`.
- [x] Re-run the focused retained-error PTY assertion and require one fresh tree
      request followed by recovery when `r` is pressed.

## 5. Decide Before Editing

- [ ] If every live check passes, record `0.33.7 child-bearing path verified;
      original failure not reproduced after reinstall` and skip source work.
- [ ] If the failure is provider, permission, timeout, or harness-owned, report
      that block and skip source work.
- [x] If a product boundary fails, replay it once from the persisted database
      without another provider run and choose the single owning test file from
      `design.md`.

## 6. TDD Fix Only If Justified

- [x] Add the smallest regression and observe the expected RED.
- [x] Apply the minimum shared root-cause fix and observe focused GREEN.
- [x] Re-run the persisted-root check, then the live slice only if offline replay
      cannot verify the fix and the user approves another provider run.
      The sanitized failure cleanup removed the persisted root; each fresh
      bounded replay received explicit approval, and the final live slice passed.
- [x] For any source fix, update `[workspace.package].version`, archive the old
      root changelog, and write the new single-version root changelog.

## 7. Verify And Clean Up

- [x] Run `cargo fmt --all --check`, workspace clippy with
      warnings denied, `cargo test --workspace`, and a local executable build.
- [x] For any TypeScript fix additionally run `bun run typecheck`, `bun test`,
      and `bun run build` from `packages/hya-tui-ts`.
      After the separately authorized permission-test alignment, the complete
      suite passes 27/27 with 2,194 assertions; typecheck and build pass.
- [x] Stop all owned processes, remove only temporary runtime artifacts, verify
      the port is closed, and compare final status with the dirty baseline.
- [x] Persist only sanitized pass/fail evidence and validate the Trellis task.
- [x] Do not commit or push unless the user explicitly requests it.

## 8. Commit, Push, And Install

- [x] Run the Trellis check agent and capture the run-tree wire contract in the
      frontend spec.
- [x] Commit the parser fix as `87bc1bea` and the independent permission-test
      alignment as `3c815452`; push both to `origin/main`.
- [x] Build release `0.33.8` from detached commit `3c815452`, install it
      atomically under `~/.local`, and verify the installed parser behavior.
