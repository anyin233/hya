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

- [ ] AC1: The coverage report exists under this task's `research/` directory and
      lists every derived feature with its status and source reference.
- [ ] AC2: Every feature in the report has status `documented`; the report's final
      pass shows zero `undocumented` and zero `thin` entries.
- [ ] AC3: Zero `stale` and zero `contradicted` entries remain.
- [ ] AC4: `cargo doc --workspace --no-deps` completes with no warnings.
- [ ] AC5: Every crate's `lib.rs` or `main.rs` opens with a `//!` comment longer
      than a restatement of the crate name.
- [ ] AC6: A `missing_docs` check over each crate's public API reports no items.
- [ ] AC7: `packages/hya-tui-ts/README.md` exists and covers install, build, run,
      and architecture; its exported symbols carry TSDoc.
- [ ] AC8: Every document reachable from `docs/README.md`; no orphan files added.
- [ ] AC9: An independent verification pass re-derives the feature surface and
      confirms coverage, rather than trusting the writers' own reports.
- [ ] AC10: `cargo build --workspace` and the repository's documentation link check
      pass; no source behavior changed (`git diff` touches only documents and
      documentation comments).

## Notes

- The audit that produces the report treats code as the source of truth and reads
  documents only in the diff step, so that what exists is discovered independently
  of what is claimed.
- `docs/opencode-feature-inventory.md` is not the checklist. It is itself audited
  against the derived surface.
