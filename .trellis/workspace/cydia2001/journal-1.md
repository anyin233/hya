# Journal - cydia2001 (Part 1)

> AI development session journal
> Started: 2026-06-20

---



## Session 1: Design and write yaca docs structure

**Date**: 2026-06-21
**Task**: Design and write yaca docs structure

### Summary

Created docs index, usage guides, detailed project structure, architecture docs, development guide, and troubleshooting guide.

### Main Changes

- Added a `docs/` documentation structure with user-facing guides, architecture notes, development guidance, and troubleshooting.
- Added a detailed project-structure guide covering workspace layout, crate responsibilities, source modules, tests, and runtime data flow.
- Kept project docs focused on yaca itself and excluded project-private workflow documentation.

### Git Commits

| Hash | Message |
|------|---------|
| `uncommitted` | (see git log) |

### Testing

- [OK] `rg -n "Trellis|\\.trellis" docs` found no matches.
- [OK] Relative Markdown link check passed.
- [OK] `cargo test --workspace` passed after rerunning one transient SQLite-lock failure.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 2: Model-specific default reasoning effort

**Date**: 2026-06-25
**Task**: Model-specific default reasoning effort
**Branch**: `main`

### Summary

Implemented native mini TUI model-specific reasoning defaults with explicit-agent, last-used, and highest-supported precedence; verified with Rust checks and TUI QA.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `b027829` | (see git log) |
| `923ed8d` | (see git log) |
| `ace5294` | (see git log) |
| `2087b4d` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 3: TUI model fallback safe rejection

**Date**: 2026-06-26
**Task**: TUI model fallback safe rejection
**Branch**: `main`

### Summary

Implemented safe native TUI direct /model rejection for unknown and ambiguous refs, added controller and harness coverage, documented the no-mutation contract, and tracked tui-check frame grouping as upstream work.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `46e144e` | (see git log) |
| `3232a44` | (see git log) |
| `4c15c0d` | (see git log) |
| `f5f8b27` | (see git log) |
| `a54cf6e` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 4: Migrate OpenCode TUI to hya-ts

**Date**: 2026-07-13
**Task**: Migrate OpenCode TUI to hya-ts
**Branch**: `main`

### Summary

Added the Bun/OpenTUI frontend and hya-ts launcher, closed SDK Compat lifecycle gaps, added install and release packaging, and passed all Rust, Bun, installer, runtime, and independent-review gates.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `44dd829e5b22eede322b918a867fb4057f9dde41` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 5: Fix hya-ts prepared runtime

**Date**: 2026-07-14
**Task**: Fix hya-ts prepared runtime
**Branch**: `main`

### Summary

Repaired the client-only prepared Bun runtime, added staged-runtime and installer regressions, shipped required runtime config, bumped to 0.33.1, and passed the full Bun/Rust/release verification gates.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `71e23bdc` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 6: Fix hya-ts subagent tree live view

**Date**: 2026-07-16
**Task**: Fix hya-ts subagent tree live view
**Branch**: `main`

### Summary

Fixed omitted live subagent summary parsing, aligned permission integration tests, pushed 0.33.8, and installed the verified local release.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `87bc1bea` | (see git log) |
| `3c815452` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 7: Commit and clean shared worktree

**Date**: 2026-07-16
**Task**: Commit and clean shared worktree
**Branch**: `main`

### Summary

Committed Trellis and project records, removed unused workspace dependencies, restored the authoritative main-agent indicator at 0.33.10, and verified the full Rust workspace.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `d8a3865c` | (see git log) |
| `c95f572a` | (see git log) |
| `580227b4` | (see git log) |
| `68a130f8` | (see git log) |
| `6a645760` | (see git log) |
| `ab01e22f` | (see git log) |
| `01b0f4fd` | (see git log) |
| `e72b1842` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 8: Add OpenAI Responses and reasoning defaults

**Date**: 2026-07-21
**Task**: Add OpenAI Responses and reasoning defaults
**Branch**: `main`

### Summary

Added configurable OpenAI Responses routing and reasoning defaults, preserved opaque reasoning through replay and tool continuation, documented and verified the cross-layer contract, and published v0.33.14.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `1dd48f6d` | (see git log) |
| `56a5a297` | (see git log) |
| `5cbf78dd` | (see git log) |
| `b2ed7889` | (see git log) |
| `d2f2f147` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 9: Configure highest reasoning effort defaults

**Date**: 2026-07-21
**Task**: Configure highest reasoning effort defaults
**Branch**: `main`

### Summary

Updated the installed hya 0.33.14 user config so all eight configured models expose explicit reasoning variants and default to their verified maximum; switched 12th-oai to the Responses route and completed config/runtime validation.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

(No commits - planning session)

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 10: Clean repository and verify outstanding work

**Date**: 2026-07-21
**Task**: Clean repository and verify outstanding work
**Branch**: `main`

### Summary

Preserved the completed reasoning-default records, removed foreign task residue, and verified the repository cleanup scope.

### Main Changes

- Preserved the completed reasoning-default task archive and Session 9 record.
- Removed the user-confirmed foreign IR task residue without publishing it.
- Archived the repository-cleanup task and recorded this session.

### Git Commits

| Hash | Message |
|------|---------|
| `2c1670d2` | (see git log) |

### Testing

- Passed task JSON/JSONL validation, exact-path staged-diff review, whitespace
  checks, foreign-path history checks, and credential-silent scans.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 11: Restore synchronous prompt working lifecycle

**Date**: 2026-07-21
**Task**: Restore synchronous prompt working lifecycle
**Branch**: `main`

### Summary

Published busy and idle lifecycle events for synchronous Compat prompts, added a pinned-SDK regression, prepared and installed hya 0.33.16, and passed full repository verification.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `dac61e8d` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 12: Configure web search providers

**Date**: 2026-07-22
**Task**: Configure web search providers
**Branch**: `main`

### Summary

Added configurable Exa and Parallel web search, enabled unauthenticated Exa by default for every model provider, documented the agent tool surface, and verified the Rust workspace.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `bfcefef9` | (see git log) |
| `64476077` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 13: Fix subagent navigation and roster shortcuts

**Date**: 2026-07-22
**Task**: Fix subagent navigation and roster shortcuts
**Branch**: `main`

### Summary

Restored Esc navigation from all subagent observation states, added shared roster shortcut docks, verified the workspace, and released version 0.33.18.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `d492135b` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 14: Fix batch task ID validation

**Date**: 2026-07-22
**Task**: Fix batch task ID validation
**Branch**: `main`

### Summary

Moved task_id validation into single-task mode, added a batch regression, released 0.33.19, and documented the task tool contract.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `2fa6a60f` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 15: Remove agent tool-call round limit

**Date**: 2026-07-22
**Task**: Remove agent tool-call round limit
**Branch**: `main`

### Summary

Removed the fixed 25-round guard from the shared session turn loop, added a 26-round regression, synchronized 0.33.28 release metadata and runtime docs, passed full workspace verification, and pushed the feature commit.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `46db2229` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 16: Make TypeScript TUI the default

**Date**: 2026-07-22
**Task**: Make TypeScript TUI the default
**Branch**: `main`

### Summary

Replaced canonical hya with the adjacent hya-ts exec path, preserved launcher/import/backend behavior, updated packaging and docs to 0.33.29, and verified all Rust, Bun, installer, and release gates.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `0fd9f80d` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 17: E2E suite hardening: land swarm branch, gate CI, cover swarm tools, measure coverage, enforce the registry

**Date**: 2026-08-06
**Task**: E2E suite hardening: land swarm branch, gate CI, cover swarm tools, measure coverage, enforce the registry
**Branch**: `main`

### Summary

Closed the four gaps from the E2E coverage audit, plus the branch landing they all depended on. Fast-forwarded 83 commits onto main after fixing 6 tests the branch shipped broken (3 of them regressions), all hidden because CI died at fmt before ever running the test step. Made every CI gate step report independently (if: !cancelled()) and proved it by observing fmt red while test still ran; Track P and the three registered Track T scenarios are now enforced, PTY smoke is explicitly non-gating. Added 6 swarm mailbox scenarios with recipient-side oracles (tool coverage 8/25 -> 14/25). Measured the first line-coverage baseline (85.56%) and reported Track P's contribution as unmeasurable, with evidence: the harness SIGKILLs the backend so LLVM never flushes profraw. Made matrix.toml enforced via cargo xtask matrix-check with bidirectional drift detection, and retired the undefined T1.1/T1.6 ids.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `16bde844` | (see git log) |
| `fa04b489` | (see git log) |
| `607c24be` | (see git log) |
| `0acfc919` | (see git log) |
| `ce7584db` | (see git log) |
| `a6ff136f` | (see git log) |
| `c315cac3` | (see git log) |
| `db2f2cc7` | (see git log) |
| `f23229a4` | (see git log) |
| `fbdad8a1` | (see git log) |
| `6cb254af` | (see git log) |
| `fee38938` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 18: Close the four E2E hardening follow-up tasks

**Date**: 2026-08-06
**Task**: Close the four E2E hardening follow-up tasks
**Branch**: `main`

### Summary

Closed all four E2E hardening follow-ups. Established root causes by measurement rather than inspection: bundle_cli temp-path collisions (nanosecond timestamps collide across simultaneously-started threads, 1336/200000 rounds -> 0 with an atomic serial); frontend_cli ETXTBSY (fs::write's write-fd inherited by a concurrent fork, O_CLOEXEC clears at exec not fork; ~3-18% -> 0/13000 via hard_link); and the hya-app admission flake family (process-global HOME feeds skill_dirs_for_workdir, whose skills are hashed into the runtime fingerprint that durable admission records at claim time and recomputes at resolve time -- 12/60 loaded runs -> 0/60 via an RwLock read guard). Catalog fixture survey found only 1 of 99 from_prepared sites can reach prepare_spawn_admission and it must stay unverified, so R2 was a no-op and the helper dedupe was the real fix; the PRD's YAML bareword hazard did not reproduce and was replaced by the hazards that do corrupt the generated manifest. e2e backend now drains on SIGTERM/SIGINT/SIGHUP -- hya-backend had no signal handler at all, and default SIGTERM disposition also skips atexit, so the harness-only fix would have flushed nothing; Track P coverage went hya-server 0.0%->21.6%, hya-core 0.0%->51.2%, hya-client 0.0%->98.3% at +2ms/backend. Governor investigation returned three decisions: the release window is a REAL user-visible defect (191/200 rejections correlating exactly with the open window, filed as 08-06-close-governor-release-window with an ignored repro asserting the intended invariant), exactness IS provable via release_operation's boolean (mutation gate: old test passed and new test failed under a real double-credit), and the drain-loop ownership invariant was recorded at its site. Version bumped 0.34.13 -> 0.34.14 across the six-site release-consistency chain. Three open flakes recorded with observation counts in agent-matrix.md, including transient_sidecar_loss_interrupts_running_member_before_provider_release which failed CI on the pre-work commit and is NOT fixed by this round.

### Main Changes

- Detailed change bullets were not supplied; see the summary above.

### Git Commits

| Hash | Message |
|------|---------|
| `dd0621f2` | (see git log) |
| `620256d6` | (see git log) |
| `d8c5a918` | (see git log) |
| `922832e7` | (see git log) |
| `0c5b0391` | (see git log) |
| `44c669eb` | (see git log) |
| `f4a88d6c` | (see git log) |
| `a1e0cac2` | (see git log) |
| `85704622` | (see git log) |
| `4c12dde8` | (see git log) |
| `3f18e6e5` | (see git log) |
| `d7724d65` | (see git log) |
| `92aebe3f` | (see git log) |
| `42afd0d6` | (see git log) |
| `15e75d3c` | (see git log) |
| `4ca89798` | (see git log) |
| `44f378e6` | (see git log) |
| `2035f1b3` | (see git log) |
| `e834a5c7` | (see git log) |
| `500c7277` | (see git log) |
| `a02a5d10` | (see git log) |

### Testing

- Validation was not recorded for this session.

### Status

[OK] **Completed**

### Next Steps

- None - task complete
