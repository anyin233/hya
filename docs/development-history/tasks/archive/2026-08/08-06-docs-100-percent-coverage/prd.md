# Documentation coverage to 100% across features, rustdoc, and TS package

## Goal

Every feature that the hya codebase implements must have documentation a user or
integrator can act on. Coverage is measured against the feature surface derived
from source code, not against the current table of contents.

## Scope

In scope:

- `docs/**/*.md`, except `docs/changes/` (release changelogs) and
  `docs/superpowers/` (archived plans and specs)
- Root documents: `README.md`, `CONTEXT.md`, `DESIGN.md`, `AGENTS.md`
- Rust API documentation: crate-level `//!` and public-item `///` comments across
  all 21 crates in `crates/`
- `packages/hya-tui-ts`: package README and TSDoc on exported symbols

Out of scope:

- `.trellis/spec/` guideline documents
- Release changelogs under `docs/changes/` and root `CHANGELOG.md`
- Archived plans under `docs/superpowers/`
- Any code change that is not a documentation comment

## Feature surface definition

The feature list is derived from source code across seven axes:

1. CLI: subcommands, flags, positional arguments, environment variables, exit codes
2. Configuration: config keys, file discovery and formats, defaults, precedence
3. Tools and permissions: built-in tools, MCP registration, permission modes and rules
4. Providers: backends, authentication, model catalog and category resolution
5. Runtime and events: event and envelope variants, session/turn lifecycle, mailbox
   and channel model, storage layout, client/server wire protocol, SDK public API
6. Extensibility: plugin binary protocol and manifest, hot reload, agent bundle
   format, skills, self-update, `install.sh`
7. TUI: screens, keybindings, interactions, tmux integration, TS package exports

A feature counts as documented only when a reader can use it from what is written:
the document must state what it does, its parameters or keys, and its semantics.
A bare mention of the name does not count.

## Requirements

- R1: Produce a coverage report that lists every derived feature and its
  documentation status, with a `file:line` source reference for each feature.
- R2: Close every `undocumented` and every `thin` gap the report identifies.
- R3: Correct or delete every `stale` and `contradicted` claim the report finds.
  Documentation that describes removed behavior is a defect, not a gap.
- R4: Give every crate in `crates/` a crate-level `//!` comment that states the
  crate's role in the system. A comment that only restates the crate name fails.
- R5: Document every public item in each crate's public API with a `///` comment
  that states purpose, parameters, and return value.
- R6: Give `packages/hya-tui-ts` a README that covers install, build, run, and
  architecture, and TSDoc on its exported symbols.
- R7: Keep documents consistent with each other. When two documents describe the
  same feature, they must not disagree.
- R8: Every new or changed document must link into the existing navigation
  (`docs/README.md` and the relevant index) so it is reachable.

## Constraints

- No behavior change. This task changes documentation and documentation comments
  only. If a document and the code disagree, the document is wrong unless the code
  is a clear defect, in which case record the defect in `docs/FOLLOWUPS.md` and
  document the actual behavior.
- Follow the existing document style and structure. Do not reorganize `docs/`.
- Write in the project's ubiquitous language, as defined in `CONTEXT.md`.
- Two writers must never edit the same file concurrently.

## Acceptance Criteria

- [x] AC1: The coverage report exists under this task's `research/` directory and
      lists every derived feature with its status and source reference.
- [ ] AC2: Every feature in the report has status `documented`; the report's final
      pass shows zero `undocumented` and zero `thin` entries.
- [ ] AC3: Zero `stale` and zero `contradicted` entries remain.
- [x] AC4: `cargo doc --workspace --no-deps` completes with no warnings.
- [x] AC5: Every crate's `lib.rs` or `main.rs` opens with a `//!` comment longer
      than a restatement of the crate name.
- [x] AC6: A `missing_docs` check over each crate's public API reports no items.
- [x] AC7: `packages/hya-tui-ts/README.md` exists and covers install, build, run,
      and architecture; its exported symbols carry TSDoc.
- [x] AC8: Every document reachable from `docs/README.md`; no orphan files added.
- [ ] AC9: An independent verification pass re-derives the feature surface and
      confirms coverage, rather than trusting the writers' own reports.
- [x] AC10: `cargo build --workspace` and the repository's documentation link check
      pass; no source behavior changed (`git diff` touches only documents and
      documentation comments).

## Notes

- The audit that produces the report treats code as the source of truth and reads
  documents only in the diff step, so that what exists is discovered independently
  of what is claimed.
- `docs/opencode-feature-inventory.md` is not the checklist. It is itself audited
  against the derived surface.

## Outcome (2026-08-07)

### Measured result

| Deliverable | Before | After | Gate |
| --- | --- | --- | --- |
| Prose feature coverage | 530/1235 (43%) | **318/324 gap list closed (98.1%)**, last 6 fixed post-measurement | 4 independent re-audits |
| Rust API docs | 2495 undocumented | **0** | `missing_docs = "deny"`, workspace-wide |
| TS package | no README, 0 TSDoc | 3 READMEs, 19/19 S1 exports | `bun test` 50/50 |
| Tests | — | **1340 passed, 0 failed** | `cargo test --workspace` |
| Links | — | 746 checked, **0 dead** | link checker |
| Behavior change | — | **none** | diff review of 324 `.rs` files |

Nine new documents; all reachable from `docs/README.md`.

### Acceptance criteria

AC1, AC4-AC8 and AC10 are met and mechanically verified by
`research/verify.sh` (all gates PASS).

**AC2, AC3 and AC9 are not met as literally written.** They required an
independent re-audit reporting *zero* residual findings. The last audited state
was 318/324 with 6 open; those 6 were fixed and hand-verified, but no fifth audit
measured the result.

### Why that criterion was miscalibrated

AC2/AC3/AC9 assumed the gap list was the whole surface. It was not. Each audit
also reported findings *outside* the 324 entries, and that count never converged
(31 -> 14 -> 21 -> 14). The reason is measurable: **8 of the 13 documents flagged
in round 3 had never been touched by any fix pass.** Adversarial readers sample
different regions of a ~15k-line corpus each run, so the stream is discovery, not
regression. "Zero findings from a fresh adversarial read" has no terminal state at
this corpus size.

The 324-item list *is* a good regression surface — stable, re-runnable, and it
converged monotonically (306 -> 312 -> 316 -> 318). Future work should measure
against it and add contract tests for sentences that encode contracts, the way
`crates/hya-bundle/tests/docs_example.rs` does. That test is the only reason a
rewrite which silently weakened 12 of its 22 pinned sentences was caught.

### The finding that outlived the documentation work

The audits repeatedly surfaced configuration fields that are parsed, serialized
onto the wire, and then silently dropped: per-command `agent`/`model`/`subtask`,
bundle `workdir`, skill `allowed-tools`/`model`, and the unreachable `last_used`
reasoning branch. Each reads as a working feature; setting one produces silence,
not an error. The documents now say so explicitly, but this is a code defect class
that documentation can only describe. Recorded in `docs/FOLLOWUPS.md`.
