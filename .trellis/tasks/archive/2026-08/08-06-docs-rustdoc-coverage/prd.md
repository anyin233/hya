# Rust API documentation coverage

Child of `08-06-docs-100-percent-coverage`. Baseline:
`../08-06-docs-100-percent-coverage/research/missing-docs-baseline.txt`.
Per-crate priorities: the "Rustdoc work" section of `coverage-gap-report.md`.

## Goal

Take the workspace from 2495 undocumented public items to zero, and give every
crate a crate-level `//!` that states its role in the system.

## Scope

In scope: `//!` and `///` comments in `crates/**`, the `missing_docs` lint
configuration in `Cargo.toml` and crate roots, and the removal of
`crates/hya-e2e/src/lib.rs`'s `#![allow(missing_docs)]`.

Out of scope: any change to code behavior, signatures, or visibility. If an item
is undocumented because it should not be public, that is recorded as a follow-up,
not fixed here.

## Baseline

| Crate | Undocumented | Crate `//!` status |
| --- | ---: | --- |
| `hya-proto` | 423 | present, strong |
| `hya-core` | 403 | present, stale ("later phases") |
| `hya-tool` | 389 | present, one line for the largest surface |
| `hya-plugin` | 266 | present, stale ("Phase 0 ships the skeleton only") |
| `hya-bundle` | 195 | present, good |
| `hya-store` | 189 | present, substantive |
| `hya-sdk` | 175 | present, one line; `reducer.rs` module doc is stale |
| `hya-app` | 108 | present, stale ("filled in during Phase 1") |
| `hya-provider` | 103 | present |
| `hya-mcp` | 72 | **absent** |
| `hya-updater` | 67 | present, strongest in the workspace |
| `hya-e2e` | 55 | present, solid; was hidden by `#![allow(missing_docs)]` |
| `hya-ts` | 23 | present but restates the crate name; `main.rs` has none |
| `hya-server` | 16 | present, thin |
| `hya-client` | 7 | present, stale consumer clause |
| `hya-plugin-compat` | 3 | **absent** |
| `hya` | 1 | **absent** |
| **Total** | **2495** | |

The gate landed first (commit `chore: enable missing_docs lint`). The count
rose from a pre-gate estimate of 2440 because removing `hya-e2e`'s blanket
`#![allow(missing_docs)]` exposed 55 items the earlier measurement could not see.

`xtask`'s `//!` reads "Dev tooling entrypoint." and explains nothing.
`hya-plugin-example`'s `//!` is aspirational rather than accurate.

By kind: 1118 struct fields, 470 enum variants, 343 methods, 202 structs, 100
associated functions, 79 enums, 44 functions, 37 modules, 21 constants, 17 traits.

## Requirements

- R1: Every public item the `missing_docs` lint reports carries a `///` comment
  stating purpose, and for functions, parameters and return value.
- R2: Every crate root opens with a `//!` explaining the crate's role in the
  system. A comment that restates the crate name fails. The five stale crate docs
  naming completed build phases are corrected, not left.
- R3: The workspace gains `missing_docs = "warn"` before the work starts, so
  progress is measurable throughout and the build stays green.
- R4: Each crate is promoted to `#![deny(missing_docs)]` in its crate root as it
  reaches zero. When all crates are at `deny`, the lint moves to the workspace
  table at `deny` and the per-crate attributes are removed.
- R5: `crates/hya-e2e/src/lib.rs`'s `#![allow(missing_docs)]` is removed. It
  currently suppresses the only automatic check that exists.
- R6: Public extension traits get contract text, not restatement — ordering,
  error semantics, and what an implementor owns. This applies to `Provider`,
  `Protocol`, `Decoder` (`hya-provider`), `Tool` (`hya-tool`), `HookDispatcher`,
  `Summarizer`, `IterationGate`, `GoalEvaluator`, `LoopVerifier` (`hya-core`),
  and `McpControl` (`hya-server`).
- R7: `hya-backend`'s `Cli` and `Command` doc comments are written knowing clap
  renders them as `--help` text.
- R8: Batches are crate-disjoint. `hya-core/src/engine/mailbox.rs` belongs to the
  `hya-core` batch alone.

## Priorities

Highest value per unit of risk, in order:

1. `hya-tool` — 134 module-level items including the whole permission state
   machine (`PermissionPlane`, `PermissionRules`, `Invocation`, `Decision`).
   Security-relevant; route `plan-executor-heavy`.
2. `hya-store` — 0% coverage, including the admission state machine. Route
   `plan-executor-heavy`.
3. `hya-core` — 74 module-level plus 128 impl methods; every public extension trait.
4. `hya-plugin` — `messages.rs` alone is ~33 undocumented wire types, the ABI an
   external plugin author reads first.
5. `hya-plugin-compat` — three doc comments complete the crate; highest doc value
   per line in the workspace.

The remainder is dominated by struct fields and enum variants on wire types and
routes to `plan-executor-bulk`.

## Acceptance Criteria

- [x] AC1: `cargo check --workspace 2>&1 | grep -c
      "missing documentation"` returns **0**.
- [x] AC2: `cargo doc --workspace --no-deps` completes with no warnings.
- [x] AC3: Every `crates/*/src/{lib,main}.rs` opens with a `//!` longer than a
      restatement of the crate name, verified by reading each one.
- [x] AC4: `missing_docs = "deny"` is in the workspace lint table and no crate
      carries a local `allow` or `deny` override for it.
- [x] AC5: `cargo build --workspace` passes and `cargo test --workspace` is no
      worse than the pre-task baseline.
- [x] AC6: `git diff` touches only doc comments and lint attributes — no
      signature, visibility, or behavior change.

## Notes

- Because each finished crate denies the lint, `cargo check -p <crate>` succeeding
  *is* the proof that crate reached zero. This is why the gate is mechanical and
  does not depend on writer self-reports.
- Risk: promoting a crate to `deny` before it is finished breaks the build.
  Promote only after the count for that crate reaches zero, verified by running
  the lint.

## Outcome (2026-08-07)

All six acceptance criteria met. **2495 -> 0 undocumented public items.**

Verification commands and results:

| Check | Command | Result |
| --- | --- | --- |
| AC1 | `cargo check --workspace --all-targets` | 0 errors |
| AC2 | `cargo doc --workspace --no-deps` | 0 warnings |
| AC3 | read every `crates/*/src/{lib,main}.rs` | 21/21 substantive `//!` |
| AC4 | `grep -rn missing_docs crates/*/src/*.rs` | 0 local overrides |
| AC5 | `cargo test --workspace` | 1340 passed, 0 failed (256 suites) |
| AC6 | diff review of all 324 changed `.rs` files | comments + lint only |

### Deviations worth recording

- **The baseline was wrong at planning time.** It was measured with
  `RUSTFLAGS="-W missing_docs" cargo check --workspace`, which (a) skips test
  targets and (b) still honoured `hya-e2e`'s `#![allow(missing_docs)]`. The true
  starting count was 2495, not 2440, and `--all-targets` later surfaced 144
  integration test files with no crate doc. Both are fixed; the lesson is that
  `--all-targets` is the only honest measurement once the lint is at `deny`.
- **Two macros gained a `$doc` parameter** (`uuid_id!`, `str_newtype!` in
  `hya-proto`). Macro-generated public types cannot take `///`, so this is the
  only way to document them. Macro bodies are otherwise unchanged.
- **Enum/struct variants expanded to multi-line.** Attaching a field doc comment
  forces a single-line variant to expand. Fields themselves are unchanged.
- **Three writer defects were caught and fixed by the orchestrator**, not by the
  writers: five files got `//!` placed mid-file (`E0753`, build-breaking); a
  duplicate doc comment and `#[must_use]` on `parse_skill`; and a rewrite of the
  `task` tool's model-facing description string, which was reverted as a product
  change and logged in `docs/FOLLOWUPS.md` instead.
- **Line-anchor fallout**: adding doc comments shifted line numbers across
  `crates/`, invalidating 157 `file:line` citations in
  `docs/architecture/agent-tool-surface.md`. Re-derived in commit
  `docs: re-derive stale source line anchors after rustdoc line shifts`.
