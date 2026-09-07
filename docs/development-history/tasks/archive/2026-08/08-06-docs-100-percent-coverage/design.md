# Design — Documentation coverage to 100%

## Measured starting point

The audit derived 1235 features from source code across seven axes and found 530
of them documented: **43% prose coverage**. It recorded 324 actionable gaps and
65 doc claims the code no longer supports.

| Area | Features | Documented | Coverage |
| --- | ---: | ---: | ---: |
| CLI | 136 | 73 | 54% |
| Configuration | 121 | 64 | 53% |
| Tools and permissions | 87 | 47 | 54% |
| Providers | 172 | 88 | 51% |
| Runtime and events | 306 | 181 | 59% |
| Extensibility | 179 | 67 | 37% |
| TUI | 234 | 10 | 4% |

Rust API documentation is separately measured: **2440 undocumented public items**
across 16 crates, of which 1118 are struct fields and 470 are enum variants.

The full report is `research/coverage-gap-report.md`. It is the work list; this
document explains how the work is organized and verified.

## Two findings that shape the design

**The TUI is the dominant prose gap.** 234 features, 10 documented. Both
`README.md:111` and `docs/getting-started.md:171` link to a keybinding reference
that does not exist. This single area is 42% of all undiscovered features.

**Rustdoc is an order of magnitude larger than the prose work.** 2440 items
against 324 prose gaps. It is also mostly mechanical: struct fields and enum
variants in `hya-proto` wire types. Mixing it into the same deliverable as the
prose work would let volume hide the harder judgment calls.

## Structure: parent with three children

The request contains three deliverables that are verified by different means and
can land independently. They become child tasks:

| Child | Deliverable | Verified by |
| --- | --- | --- |
| `docs-prose-coverage` | 324 gaps + 65 stale claims across `docs/**` and root documents | Independent re-audit against the derived feature surface |
| `docs-rustdoc-coverage` | 2440 public items + 3 missing crate `//!` | `missing_docs` lint reaching zero, per crate |
| `docs-ts-package` | `packages/hya-tui-ts` READMEs and TSDoc | File existence plus export-coverage check |

Ordering is not enforced by the tree. The one real dependency is written into the
child artifacts: the prose child's final reconciliation step adds the new document
paths to `docs/README.md`, so it must finish after its own content batches. The
three children do not touch each other's files and may run concurrently.

## File-disjoint batching

Every batch owns a set of files, and no file appears in two batches. This is what
makes parallel writers safe; it is not an optimization, it is the correctness
condition. The audit produced the assignment and flagged the one collision it
found (`hya-core/src/engine/mailbox.rs` claimed by both Q2 and Q11), resolved by
folding Q11's mailbox item into Q2.

Batches are paired where two documents must agree with each other, so one writer
reconciles both rather than two writers drifting:

- `storage.md` + `admission-and-governor.md` — schema and API must match
- `tools-and-permissions.md` + `agent-tool-surface.md` — these currently
  **contradict** each other on `write`/`edit` schemas and the builtin inventory
- `tui-keybindings.md` + `tui-reference.md` — same keymap source
- `plugin-protocol.md` + `compat-plugins.md` — adapter implements the protocol
- `agent-bundle-authoring.md` + `skills.md` — `resources.skills` overlaps discovery

## Enforcement over self-report

A writer agent reporting "documented" is not evidence. Each deliverable gets a
mechanical gate:

**Rustdoc.** Add `missing_docs` to the workspace lint table. It starts at `warn`
so the build stays green during the work, and each crate is promoted to `deny` in
its own crate root as that crate reaches zero. When every crate is at `deny`, the
lint is moved to the workspace table at `deny` and the per-crate attributes are
removed. `hya-e2e`'s existing `#![allow(missing_docs)]` is removed, not kept —
it currently suppresses the only automatic check that exists.

**Prose.** Re-run the audit workflow after the writes, from the same source-derived
feature list, with fresh agents that did not write the documents. A writer cannot
mark its own work complete. Coverage is the re-audit's number, not the writer's.

**TypeScript.** A check that every exported symbol in the audited files carries a
docblock, plus existence of the three READMEs.

## New documents

Nine paths do not exist today. Each is justified by gaps that have no sensible
home in an existing file:

| Path | Why it must be new |
| --- | --- |
| `docs/tui-keybindings.md` | Two existing documents already link to it |
| `docs/tui-reference.md` | `architecture/tui.md` is architecture-only; no user-facing TUI page exists |
| `docs/plugin-protocol.md` | The plugin JSON-RPC ABI appears in no prose at all |
| `docs/compat-plugins.md` | Bundled Bun adapter has one line of mention |
| `docs/skills.md` | User-authorable, zero authoring documentation |
| `docs/architecture/admission-and-governor.md` | 2223 lines of safety-critical spawn-budget code with no doc comments and no prose |
| `packages/hya-tui-ts/README.md` | Package cannot be launched from what is written |
| `packages/hya-tui-ts/scripts/README.md` | `prune-sdk-server.ts` runs in `install.sh` and CI, undocumented |
| `packages/hya-tui-ts/test/README.md` | Three suites are invariant guards with confusing failure messages |

All nine enter `docs/README.md`'s Docs Map in the reconciliation step, satisfying
the no-orphan requirement.

## Stale content is a defect, not a gap

65 entries describe behavior the code no longer has. These are corrected or
deleted, never merely supplemented — a document that contradicts the code is worse
than a missing one, because a reader trusts it. Where a document and the code
disagree and the *code* looks wrong, the writer documents actual behavior and
records the suspected defect in `docs/FOLLOWUPS.md` rather than silently
describing intended behavior.

## Compatibility and rollback

No source behavior changes. The only non-document edits are lint attributes
(`missing_docs` levels) and the removal of `hya-e2e`'s blanket allow. Rollback is
per-batch: each batch is one commit touching a disjoint file set, so any batch can
be reverted without disturbing another.

Risk of the lint change: promoting to `deny` before a crate is finished breaks the
build. Mitigated by promoting only after that crate's count reaches zero, verified
by running the lint rather than by trusting the writer.

## What this design rejects

**Documenting from the existing table of contents.** The audit read code first for
exactly this reason. `docs/opencode-feature-inventory.md` is audited as an input,
not used as the checklist — it is itself listed among the files carrying stale
claims.

**One writer per document tree.** Sequential writing of 324 gaps and 2440 items is
the same work with none of the parallelism and no better consistency, since
consistency is enforced by the paired-batch rule and the final reconciliation pass.

**Counting a name-mention as documented.** The audit's `thin` status exists to
catch this, and those entries are in scope for R2.
