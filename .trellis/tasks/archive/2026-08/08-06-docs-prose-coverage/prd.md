# Prose documentation coverage

Child of `08-06-docs-100-percent-coverage`. Work list:
`../08-06-docs-100-percent-coverage/research/coverage-gap-report.md`.

## Goal

Close all 324 prose documentation gaps and correct all 65 stale or contradicted
claims across `docs/**` and the root documents, taking prose coverage from 43% to
100% of the source-derived feature surface.

## Scope

In scope: `docs/**/*.md` except `docs/changes/` and `docs/superpowers/`; root
`README.md`, `CONTEXT.md`, `DESIGN.md`, `AGENTS.md`. Includes the six new document
paths listed below.

Out of scope: Rust doc comments (child `docs-rustdoc-coverage`), the
`packages/hya-tui-ts` documents (child `docs-ts-package`), any source change.

## Starting coverage

| Area | Features | Documented | Coverage |
| --- | ---: | ---: | ---: |
| CLI | 136 | 73 | 54% |
| Configuration | 121 | 64 | 53% |
| Tools and permissions | 87 | 47 | 54% |
| Providers | 172 | 88 | 51% |
| Runtime and events | 306 | 181 | 59% |
| Extensibility | 179 | 67 | 37% |
| TUI | 234 | 10 | 4% |

## New documents required

| Path | Reason |
| --- | --- |
| `docs/tui-keybindings.md` | `README.md:111` and `docs/getting-started.md:171` already link to a reference that does not exist |
| `docs/tui-reference.md` | No user-facing TUI page exists; `architecture/tui.md` is architecture-only |
| `docs/plugin-protocol.md` | The plugin JSON-RPC ABI appears in no prose |
| `docs/compat-plugins.md` | The bundled Bun adapter has one line of mention |
| `docs/skills.md` | User-authorable, with zero authoring documentation |
| `docs/architecture/admission-and-governor.md` | 2223 lines of safety-critical spawn-budget code with no prose |

## Requirements

- R1: Every gap entry assigned to this child's files reaches status `documented` —
  the reader can use the feature from what is written, including parameters or
  keys and semantics. A name-mention does not satisfy this.
- R2: Every `stale` and `contradicted` entry is corrected or deleted, not
  supplemented. A document that contradicts the code is a defect.
- R3: Every claim is confirmed against the `file:line` source reference before it
  is written. A writer that cannot confirm a claim from source reports that it
  could not, rather than writing plausible prose.
- R4: Paired documents agree with each other. `tools-and-permissions.md` and
  `agent-tool-surface.md` currently contradict each other on `write`/`edit`
  schemas and the builtin inventory; one writer reconciles both.
- R5: Batches are file-disjoint. No two writers edit the same file.
- R6: Any code defect discovered while writing is recorded in `docs/FOLLOWUPS.md`;
  the document describes actual behavior, not intended behavior.
- R7: All six new paths are added to the Docs Map in `docs/README.md`, and
  `docs/skills.md` plus `docs/tui-keybindings.md` to the "If you want to run hya"
  reading path.
- R8: Existing document style and structure are preserved. `docs/` is not
  reorganized. Terminology follows `CONTEXT.md`.

## Batches

Wave 1 batches A–N and P from the report, file-disjoint, plus the Wave 4
reconciliation pass which runs last and single-owner.

Routed to `plan-executor-heavy`: E (`event-model.md`, a wire contract), F
(`storage.md` + `admission-and-governor.md`, safety-critical and must agree), G
(`tools-and-permissions.md` + `agent-tool-surface.md`, actively contradictory).
All other batches route to `plan-executor-bulk`.

## Acceptance Criteria

- [ ] AC1: **Not met as literally written.** Last independent measurement
      (round 4) was **318/324 closed, 6 open** (5 `thin`, 1 `contradicted`). A
      fifth pass fixed all 6 and each was hand-verified against source, but no
      fifth audit ran, so the last audited figure stands. See "Why this criterion
      was miscalibrated" below.
- [ ] AC2: **Not met as literally written.** Same measurement. Zero `stale`
      remained; 1 `contradicted` (`finalize_root_spawn_admissions` guard) was
      fixed and hand-verified post-measurement.
- [x] AC3: All six new documents exist and are reachable from `docs/README.md`.
- [x] AC4: Every relative link in `docs/**` resolves.
- [x] AC5: `git diff` for this child touches only Markdown files.
- [x] AC6: Coverage is reported as the re-audit's number. Partial coverage is
      reported as that number, never as "done".

## Notes

- The re-audit is the acceptance evidence. Writer self-reports do not close AC1
  or AC2.
- `docs/opencode-feature-inventory.md` is a target of correction, not a checklist.
  It carries stale claims at `:16` and `:17`.

## Outcome (2026-08-07)

Prose coverage went from **43% (530/1235 features)** to a closed 324-item gap list.

### Measured by independent re-audit

Three audits, each by agents that did not write the documents, re-checking all 324
entries against the current docs AND the source:

| Round | Closed | Still open | Findings outside the list |
| --- | ---: | ---: | ---: |
| 1 | 306/324 | 18 | 31 |
| 2 | 312/324 | 12 | 14 |
| 3 | 316/324 | 8 | 21 |

| 4 | 318/324 | 6 | 14 |

**Final independently measured figure: 318/324 (98.1%).** A fifth correction pass
(J1-J4) closed the remaining 6; each was verified by reading the source directly
before committing:

| Item | Source ground truth | Doc now says |
| --- | --- | --- |
| HTTP routing fingerprint | `http.rs:410` appends `bearer-resolver-slot` + presence byte | 12th component documented with presence semantics |
| Root-turn admission cleanup | `turn.rs:508` guards on `self.governor.is_some()` | "on a **governor-backed** engine", guard shown |
| TUI state files | `paths.state/kv.json`, `paths.state/session.json` | both named, with the full KV flag list |
| `/export` | `openEditor` called in BOTH branches (`index.tsx:1126`, `:1142`) | editor-opens-and-writes-back documented |
| `resource_view.namespace` | applies only to `bundle:<ns>/<kind>/<short>` | corrected from "prefix for public names" |
| Tool renderers | `write`/`question` use `BlockTool`, not inline rows | per-renderer icons and labels documented |

AC1/AC2 remain formally unmet because no audit measured the post-fix state. That
is the honest position: this task never accepted a writer's word as evidence, and
does not start now.

### Nine new documents

`tui-keybindings.md`, `tui-reference.md`, `skills.md`, `plugin-protocol.md`,
`compat-plugins.md`, `architecture/admission-and-governor.md`, and three under
`packages/hya-tui-ts/`. All reachable from `docs/README.md`; 746 relative links
checked, 0 dead.

### Why the acceptance criterion was partly wrong

AC1/AC2 asked for zero findings from a fresh adversarial read. That is
unreachable for a ~15k-line corpus: the audits kept surfacing pre-existing issues
in regions they had not previously sampled (8 of 13 documents flagged in round 3
had never been touched by a fix pass). The 324-item list is the stable regression
surface; open-ended adversarial reading is a discovery process with no terminal
state. Recorded in `docs/FOLLOWUPS.md` for whoever continues.

### What the audits kept finding

A defect class, not a writing problem: configuration fields that are parsed,
serialized, and silently dropped (per-command `agent`/`model`/`subtask`, bundle
`workdir`, skill `allowed-tools`/`model`, the `last_used` reasoning branch). The
documents now say so explicitly rather than describing intent.
