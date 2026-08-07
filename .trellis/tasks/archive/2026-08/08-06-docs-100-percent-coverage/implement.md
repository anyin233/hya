# Implementation plan — Documentation coverage to 100%

Work list: `research/coverage-gap-report.md`. Baseline lint output:
`research/missing-docs-baseline.txt`.

This parent task owns sequencing, the shared gates, and the final integration
review. The three child tasks own the writing.

## Step 0 — Preconditions

- [ ] `git status` is clean for every path this task will touch, or the unrelated
      dirty paths listed in the session snapshot are stashed. The working tree
      currently carries 18 dirty paths from prior work; none may be swept into a
      documentation commit.
- [ ] Record the baseline so the final gate can prove movement:
      `RUSTFLAGS="-W missing_docs" cargo check --workspace 2>&1 | grep -c "missing documentation"` → expect **2440**.

## Step 1 — Land the enforcement mechanism

**Deferred to the start of the rustdoc child, not run now.** The original ordering
assumed the rustdoc work followed immediately. Since the prose child runs first,
landing `missing_docs = "warn"` now would inject 2440 warnings into every `cargo`
invocation the writer makes during prose work, for no benefit — the prose child's
gate is the re-audit, not the lint. The gate still lands before the first rustdoc
batch, which is what makes rustdoc progress measurable.

- [ ] Add to root `Cargo.toml`:
      ```toml
      [workspace.lints.rust]
      missing_docs = "warn"
      ```
- [ ] Confirm each crate inherits it (`[lints] workspace = true` in each crate
      manifest); add where absent.
- [ ] Remove `#![allow(missing_docs)]` from `crates/hya-e2e/src/lib.rs`.
- [ ] Verify: `cargo check --workspace` still succeeds (warnings only, no errors).

Validation: `cargo check --workspace 2>&1 | grep -c "missing documentation"`
returns a number greater than zero and the build does not fail.

Rollback point: revert this commit and the tree is exactly as it started.

## Step 2 — Child A: prose documentation

Owner: child task `docs-prose-coverage`. Wave 1 of the report, 16 file-disjoint
batches (A–N, P, R minus the package batch, which belongs to child C).

Dispatch rule per `CLAUDE.md`: these are content-authoring steps with no behavior
risk, so they route to `plan-executor-bulk`, **except** batches E, F, and G, which
route to `plan-executor-heavy`:

- **E** (`event-model.md`) — the event catalog is a wire contract; a wrong claim
  misleads integrators.
- **F** (`storage.md` + new `admission-and-governor.md`) — safety-critical spawn
  budget state machine, and the schema and API must agree.
- **G** (`tools-and-permissions.md` + `agent-tool-surface.md`) — these currently
  contradict each other; resolving which is right requires reading the code, not
  picking one.

Every writer receives: its batch's gap entries verbatim from the report, the
`file:line` source references, and the instruction to read the source before
writing. A writer that cannot confirm a claim from source must say so rather than
write plausible prose.

- [ ] Batches A–N, P dispatched, file-disjoint, each one commit.
- [ ] Every `stale` and `contradicted` entry for the batch's files corrected or
      deleted, not supplemented.
- [ ] Any code defect discovered recorded in `docs/FOLLOWUPS.md`.

## Step 3 — Child B: rustdoc

Owner: child task `docs-rustdoc-coverage`. Wave 2, 12 sub-batches, one crate (or
small crate group) per writer, never overlapping files.

Priority order by size and risk: Q1 `hya-tool` (134, security-relevant), Q2
`hya-core` (74 + 128 methods), Q3 `hya-store` (0% coverage), then the rest.

Route `plan-executor-heavy` for Q1 (permission state machine) and Q3 (admission
state machine); `plan-executor-bulk` for the remainder, which is dominated by
struct fields and enum variants on wire types.

Per crate, in order:

- [ ] Add or rewrite the crate-level `//!`. Three crates have none: `hya`,
      `hya-mcp`, `hya-plugin-compat`. Five have stale ones naming build phases
      that have passed (`hya-app`, `hya-core`, `hya-plugin`, `hya-client`,
      `hya-plugin-example`) — those are corrections, not additions.
- [ ] Document every public item the lint reports for that crate.
- [ ] Promote that crate to `#![deny(missing_docs)]` in its crate root.
- [ ] Validation gate for the crate: `cargo check -p <crate>` succeeds. Because
      the crate now denies the lint, success *is* the proof of zero.

Known collision, already resolved: `hya-core/src/engine/mailbox.rs` belongs to Q2
only. Q11 writes `hya-sdk/src/reducer.rs` alone.

## Step 4 — Child C: TypeScript package

Owner: child task `docs-ts-package`. Wave 3 plus batch R.

- [ ] `packages/hya-tui-ts/README.md`, `scripts/README.md`, `test/README.md`.
- [ ] TSDoc batches S1 (hya-owned surface), S2 (two largest hya-authored files
      under `src/upstream`), S3 (72 exports, 0 docblocks).
- [ ] Validation: `bun run typecheck` and `bun test` in the package still pass.

## Step 5 — Reconciliation (single writer, runs last)

Wave 4. Every file whose correctness depends on the new documents existing.
Sequential and single-owner by construction — this is where cross-document
consistency (R7) and the no-orphan rule (R8) are satisfied.

- [ ] `docs/README.md` — all nine new paths into the Docs Map and the reading paths.
- [ ] `README.md` — repoint `:111` at `docs/tui-keybindings.md`; scope the `:79-80`
      completeness claim; fix the `:74-77` `hya-ts` block.
- [ ] `AGENTS.md`, `DESIGN.md`, `docs/project-structure.md`, `docs/compat-parity.md`,
      `docs/opencode-feature-inventory.md`, `docs/hya-pi-compat-comparison.md`.
- [ ] Every relative link in `docs/**` resolves.

## Step 6 — Verification gate

Nothing is complete until an independent pass confirms it. Writers do not close
their own acceptance criteria.

- [ ] **Rustdoc**: `cargo doc --workspace --no-deps` completes with no warnings
      (AC4). `RUSTFLAGS="-W missing_docs" cargo check --workspace 2>&1 | grep -c
      "missing documentation"` returns **0** (AC6).
- [ ] **Crate docs**: every `crates/*/src/{lib,main}.rs` opens with a `//!` that
      is more than a restatement of the crate name (AC5) — checked by reading, not
      by grep, since grep cannot judge "more than a restatement".
- [ ] **Prose**: re-run the audit workflow with fresh agents against the same
      source-derived feature list. Zero `undocumented`, zero `thin` (AC2), zero
      `stale`, zero `contradicted` (AC3). This is AC9 — the re-audit is the
      acceptance evidence, not the writers' reports.
- [ ] **Package**: the three READMEs exist and cover the required sections;
      exported symbols carry TSDoc (AC7).
- [ ] **No orphans**: every document reachable from `docs/README.md` (AC8).
- [ ] **No behavior change**: `cargo build --workspace` passes and `git diff
      --stat` shows only documents, doc comments, and the lint attributes (AC10).

If the re-audit finds residual gaps, they return to the owning child task as a
second pass. Partial coverage is reported as a number, never as "done".

## Step 7 — Commit and wrap-up

Per `AGENTS.md`: one commit per atomic batch, staging only that batch's files,
one-line semantic messages, no agent attribution. Documentation-only changes do
not require the TDD gate, but do require their verification command to have run.

Version and changelog: **no version bump, no `CHANGELOG.md` edit.** Decided at
review — this task changes no product behavior, and root `CHANGELOG.md` must hold
only the newest release's notes because the release workflow reads it verbatim.

## Execution model — Grok in a herdr pane

Decided at review: the documentation is written by the **Grok Build TUI** driven
in a dedicated `herdr` pane, not by in-process subagents.

Environment (confirmed, not assumed): this session runs inside herdr workspace
`w8`, tab `w8:t1`, pane `w8:p2`, server 0.7.5 protocol 17 on
`~/.config/herdr/herdr.sock`. `grok` resolves to `~/.grok/bin/grok` and is a
supported `herdr agent start --kind` value.

Drive loop per batch:

```
herdr pane split --current --direction right --cwd <repo>   # once, returns pane id
herdr agent start <name> --kind grok --pane <id>            # once
herdr agent prompt <target> "<batch brief>" --wait --until idle --timeout <ms>
herdr agent read <target>                                   # collect result
```

Rules for this loop:

- One batch per prompt. The batch brief carries the gap entries verbatim and the
  `file:line` source references. Grok executes a batch; it does not re-plan.
- `--wait --until idle` gates each batch. Note the documented caveat: `--wait`
  does not track turns, so if the agent is already working, that turn's completion
  may match. Confirm with `herdr agent read` before treating a batch as finished.
- Verify each batch from this session — read the diff, run the batch's validation
  command. Grok's own report is not the gate.
- Commit per batch from this session, staging only that batch's files.

Pane count starts at one. Scale to more panes only after the loop is proven end to
end on the first batch.

## Open questions for review

*(Both resolved at review: no version bump; scale approved.)*
3. ~~**Dirty tree.**~~ **Resolved during planning.** The uncommitted
   `crates/hya-sdk/src/{reducer,store,types}.rs` edits are test-fixture path moves
   (`fixtures/` → `tests/fixtures/`) confined to `mod tests` blocks. They do not
   touch the module-level `//!` that Q11 rewrites, so the two do not conflict.
   Leave them uncommitted and stage only documentation lines. Confirmed separately
   that the stale claim is real: `reducer.rs:264` `apply` has a full match body,
   while the module doc still calls it "a no-op skeleton".


---

## Execution record (2026-08-06 -> 2026-08-07)

Executed by driving the **Grok Build TUI** in four `herdr` panes in workspace `w8`
(`docs-writer`, `docs-w2`, `docs-w3`, `docs-w4`), one batch per prompt, with the
orchestrator verifying every diff against source before committing.

### What landed

| Wave | Batches | Result |
| --- | --- | --- |
| 1 - prose | A,B,C,D,E,F,G,H,I,J,K,L,M,N,P | 15 batches, 9 new documents |
| 2 - rustdoc | Q1..Q12 | 2495 -> 0 undocumented items |
| 3 - TS package | S1 + 3 READMEs | 19/19 exports, S2/S3 vendored-excluded |
| 4 - reconciliation | Wave 4 | docs map + cross-cutting corrections |
| fixes | F1..F6, G1..G6 | 12 batches driven by two independent re-audits |

### Verification actually performed

Three independent re-audits (`research/reaudit.json`, `reaudit2.json`, and the
third run), each using agents that did not write the documents, re-checking all
324 original gap entries against the current docs AND the source.

| Round | Closed | Still open | Contradictions introduced | Critic findings |
| --- | ---: | ---: | ---: | ---: |
| 1 | 306/324 | 18 | 31 | 9 |
| 2 | 312/324 | 12 | 14 | 14 |

The writers' own reports were never accepted as evidence. Round 1 proved why:
the writers believed their batches were complete, and had introduced 31 fresh
contradictions in the process.

### Orchestrator mistakes made and corrected

Recorded because they are the reusable lessons, not the doc content.

1. **Lossy work list.** The synthesized gap report omitted per-file gap sections
   for eight documents. Briefs built from it would have closed ~180 fewer gaps
   while looking complete. Fixed by recovering all 324 raw entries from the
   workflow journal and rebuilding every brief from those.
2. **Swept another task's WIP into commits, twice.** A broad `git add crates/`
   pulled in an unrelated `fixtures/` -> `tests/fixtures/` rename whose target
   directory is untracked, leaving HEAD pointing tests at files not in the repo.
   Fixed with explicit pathspec exclusions; `AGENTS.md` already forbids this.
3. **Measured a gate with the wrong command.** AC1 was reported green using
   `cargo check --workspace`, which skips test targets; `--all-targets` then
   failed to compile. Never trust a narrower command than the criterion.
4. **Read an exit code instead of the output.** `cargo test` returned 0 while a
   target failed to compile.
5. **Broke a documentation contract test.** A rewrite paraphrased 12 of 22
   sentences that `crates/hya-bundle/tests/docs_example.rs` asserts verbatim,
   weakening real contract terms. Only the test caught it.
6. **Invalidated 157 source line anchors.** Adding doc comments shifted line
   numbers; `docs/architecture/agent-tool-surface.md` cited them heavily.
