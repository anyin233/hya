# Synchronize all project documentation

## Goal

Make the current project documentation describe the checked-out `0.36.7` source
tree and shipped boundaries truthfully. Users, maintainers, and coding agents
must be able to follow the documented CLI, HTTP/API routes, Workflow routing,
provider behavior, tool inventory, TypeScript TUI ownership, package payload,
and test/release gates without encountering claims from removed components or
older releases. Historical evidence remains readable and unchanged.

## Background and confirmed evidence

- The Rust workspace version is `0.36.7`, with edition 2024, Rust `1.91`, and
  `MIT OR Apache-2.0` workspace metadata (`Cargo.toml:8-12`). The latest public
  release is `v0.35.1`, while the checked-out `0.36.7` main line is not a crates.io
  package (`findings.md:39`). Documentation must distinguish those facts.
- The runtime boundary is `hya` exec shim -> `hya-ts` launcher/supervisor ->
  `hya-backend` runtime/HTTP-SSE server -> `packages/hya-tui-ts` TypeScript
  frontend. Workflow authoring/normalization belongs to `hya-workflow`, durable
  execution to `hya-core`, control/admission to `hya-app`, and package models to
  `hya-bundle` (`findings.md:26,42,54,57`).
- Native session routes include `/sessions/:id/{prompt,command,shell,workflow}`
  plus `/sessions/:id/{events,stream}` (`crates/hya-server/src/lib.rs:49-75`).
  Compat routes are a separate SDK surface and must not be presented as native
  parity (`findings.md:27,86-87`).
- Workflow stage routing is request-local, ordered, pre-stream-fallback only,
  and records bounded route outcomes for replay. The current model-routing
  release note is `0.36.7` (`CHANGELOG.md:1-10`; `findings.md:28,47-48,60-61`).
- `ToolRegistry::builtins()` registers 28 canonical schemas, including
  `workflow` and `announce`; the matrix table at
  `docs/testing/agent-matrix.md:68-75` is the current count reference.
- Installation and release artifacts contain three Rust binaries, the prepared
  TypeScript runtime, and `lib/hya/compat-adapter`; release also packages the
  Argus WorkflowBundle example (`install.sh:22-28,205-223`;
  `.github/workflows/release.yml:92-130`; `findings.md:30`).
- The installer copies and verifies `NOTICE` (`install.sh:208-211,261-271`),
  while the `0.36.7` GitHub release workflow and release rehearsal copy only
  `LICENSE` and `UPSTREAM.md` (`.github/workflows/release.yml:117-126`;
  `crates/xtask/src/release_rehearsal.rs:793-797`). `UPSTREAM.md` directs
  packaged readers to `NOTICE`; B3 must state this observed difference in prose
  without changing packaging behavior.

- CI gates typecheck/build, focused TUI tests, matrix registration, Rust checks,
  and Track P E2E; full Bun/PTY coverage is intentionally non-gating in current
  workflow policy (`.github/workflows/ci.yml:35-102`; `findings.md:31`).
- The read-only independent inventory inspected 681 tracked Markdown files,
  excluded 342 historical/vendor/managed/fixture files, classified 339 current
  candidates, and scanned 92 high-priority documents for relative links. It
  found no missing relative Markdown target in that primary scan. Its findings
  are merged into `findings.md:131-182` and retained in
  `research/independent-inventory.md`.

## Requirements

### R1. Evidence-backed audit and claim ledger

Maintain a repository-grounded claim ledger in the active task findings. Every
confirmed stale or incomplete claim must name the document path, line/heading
anchor, current source/test/help/config/release evidence, severity, and planned
action. Separate objective drift from owner decisions and mark historical or
managed evidence that must not be rewritten.

### R2. Current root, glossary, index, and component documentation

Synchronize `README.md`, `DESIGN.md`, `CONTEXT.md`, `CHANGELOG.md`,
`docs/README.md`, `docs/development.md`, and `docs/project-structure.md` with
the runtime chain, Workflow ownership, queued/overlay prompt behavior,
semantic border rules, category registry ownership, model-route terminology,
release/version status, tool count, client/transport boundaries, and xtask
invocations. Preserve root changelog discipline: only the newest release notes
remain in `CHANGELOG.md`; historical notes stay under `docs/changes/`.

### R3. Current architecture, API, provider, Workflow, tool, and Compat docs

Synchronize the affected pages under `docs/architecture/`, `docs/cli.md`,
`docs/configuration.md`, `docs/workflows.md`, `docs/compat-parity.md`, and
`docs/plugin-protocol.md`. Keep `docs/architecture/server-client.md` as the
canonical route table; cross-link rather than duplicate exhaustive route lists.
Document 28 tools, Workflow and Skill contributions, the 54-variant event model,
durable `ContextCompacted` checkpoints, `WorkflowStageRouteOutcome`, explicit
route versus engine fallback semantics, adapter resolution precedence, and
actual CLI/help command forms. Keep native-only extensions distinct from Compat
parity.

### R4. TypeScript TUI, install, package, and self-update documentation

Synchronize `docs/architecture/tui.md`, `docs/tui-reference.md`,
`docs/tui-keybindings.md`, `docs/getting-started.md`,
`docs/agent-bundle-authoring.md`, `docs/examples/self-update/README.md`,
`docs/adr/0005-drop-legacy-tui-surface.md`,
`docs/adr/0006-tui-session-reset-and-subagent-visibility.md`,
`packages/hya-tui-ts/README.md`, `packages/hya-tui-ts/UPSTREAM.md`, and
`packages/hya-tui-ts/test/README.md`.
State that the shipped interactive frontend is TypeScript/OpenTUI only; update
the 12-declared/11-default plugin inventory and Workflow sidebar; document the
14-file test inventory and Workflow test requirements; describe the prepared
runtime subset, Compat adapter payload, deterministic `package-bundle` writer,
checkout versus installed launcher invocation, and self-update stage/discard
sequence. In package prose, state explicitly that `install.sh` includes and
verifies `NOTICE`, but the `0.36.7` release workflow/rehearsal does not; this is
an observed packaging difference, not a request to change packaging. Do not
imply that the standard release tarball ships `hya-updater`.


### R5. Testing, release, and active Trellis guidance

Synchronize `docs/testing/{README.md,agent-matrix.md,coverage-baseline.md,
process-e2e.md}`, applicable `.trellis/spec/backend/` and
`.trellis/spec/frontend/` guides, `.trellis/workflow.md`, and the code-reuse
guide's project-local/template boundary. Add T2.14 evidence, use the T0-T3
range, label dated coverage as a baseline, update current flake evidence with
stable symbols, document CI's resource cap, and expand release verification to
the adapter/Argus payload and the pinned no-publish rehearsal. Correct stale
archived-task and nonexistent-path references without touching archived files.

### R6. Wiki and package README consistency

Audit `.autors/hya/wiki/{INDEX.md,README.md,pages/**/*.md}` and all package/crate
README files. Edit only current claims that contradict source or current docs;
keep Agent-authored or machine-specific wiki material separate when its current
environment cannot be verified. Update indexes when a child document moves or
changes, and use canonical docs as links instead of copying large contracts.

### R7. Safety and clean boundaries

This task is documentation synchronization only. Do not change Rust, TypeScript,
tests, generated assets, release behavior, source usage strings, package
payload behavior, or product configuration. Do not alter `.argus_subagents/**`,
archived Trellis tasks, `docs/changes/CHANGELOG_*.md`, historical superpower
plans, dated coverage measurements, vendor/generated/node_modules/target, or
workspace journals. Reference-only stale paths in source/config files are
recorded for follow-up unless an owner explicitly expands scope.

### R8. Tracked agent instructions

`AGENTS.md` is in scope because this request covers all current project
documents. Its corrections are a dedicated B5 sub-batch, not ordinary Markdown
editing: load `agent-dotfile` and `writing-for-agents`, preserve the managed
Trellis block, update only the intended current component/verification guidance,
and run the install/sync verification required by those workflows. Do not edit
`AGENTS.md` during this planning pass; execution must use the named workflows.


## In scope

- The current documents and specs named in R2-R6, `AGENTS.md`, and their
  directly affected indexes and cross-links.
- Objective corrections for confirmed P0/P1 drift and broken anchors/paths.
- Explicit wording that distinguishes current source behavior, dated evidence,
  Compat-only behavior, and unresolved owner decisions.
- A review of current documents with no confirmed drift; leave those documents
  unchanged rather than rewriting them for activity.
- A final repository-wide documentation reconciliation and targeted contract
  verification after implementation.


## Out of scope

- Product/code/config behavior changes, including source usage aliases, release
  `NOTICE` copying, provider retry constants, compiler qualification rules,
  queued-prompt command handlers, or adding matrix-gating tests.
- Rewriting historical changelogs, ADR rationale, archived task artifacts,
  superpower plans, dated coverage tables, prompt/fixture sources, managed
  platform mirrors, or machine-specific operational evidence without owner
  approval.
- Regenerating coverage, exhaustive generated OpenAPI/Compat route inventories,
  legal licensing policy, version synchronization policy, or toolchain policy.

## Deferred owner decisions (non-blocking)

The plan uses conservative documentation-only recommendations, so none of these
blocks the implementation batches:

1. **License and NOTICE payload:** describe observed workspace/package licenses and
   do not claim absent root license files are in the release archive.
   `packages/hya-tui-ts/UPSTREAM.md` must state that `install.sh` includes and
   verifies `NOTICE`, while the `0.36.7` release workflow/rehearsal does not.
   A future packaging decision may add `NOTICE` to release output, but this task
   changes prose only and does not alter packaging (`findings.md:49,146`).
2. **Version ownership:** report `0.36.7` for Rust/TUI and `0.0.0`/`local` for
   private adapter/runtime metadata; do not invent a synchronization rule
   (`findings.md:50`).
3. **Workflow model IDs:** call “fully-qualified” a documented convention, not
   a compiler invariant, unless a product task changes validation
   (`findings.md:84`).
4. **Provider/retry detail:** enumerate configured provider kinds when claiming
   an exhaustive list, but describe retry/fallback semantics without exposing
   internal numeric limits unless an owner requests them (`findings.md:61,85`).
5. **Compat parity scope/routes:** keep `docs/compat-parity.md` Compat-only and
   keep one canonical native route table; link generated route outputs only if a
   later owner decision requires exhaustive coverage (`findings.md:86-87`).
6. **Queued prompts and T3 Workflow TUI tests:** document the existing accepted
   key definition as not currently wired, and classify Workflow TUI suites as
   package smoke/non-gating unless product owners accept a handler or flake-risk
   change (`findings.md:99,107`).
7. **Wiki toolchain policy:** leave machine-specific pin and package behavior as
   explicit follow-up decisions, not inferred current guarantees
   (`findings.md:80,93,100`). The `NOTICE` discrepancy is not deferred wording:
   B3 records the installer/release difference explicitly while leaving
   packaging unchanged.


**Blocking open questions:** none for the documentation-only plan. The deferred
decisions are recorded so implementation does not silently change product,
legal, compatibility, or release contracts.

## Acceptance Criteria

- [x] **A1 — Complete evidence ledger:** `findings.md` and the independent
  inventory identify every confirmed P0/P1 documentation drift in the audited
  current scope, with path/line anchors, source evidence, action, and immutable
  or deferred boundaries; no seed-only context manifest remains.
- [x] **A2 — Root and architecture truth:** root/index/glossary/component maps
  describe the `hya` -> `hya-ts` -> backend -> TypeScript TUI chain, the
  `hya-workflow`/`hya-core`/`hya-app`/`hya-bundle` boundaries, current version
  status, prompt/border semantics, and client/transport ownership without
  contradicting source.
- [x] **A3 — Runtime/API truth:** CLI synopsis/help, slash catalog, native route
  links, adapter precedence, provider/fallback wording, Workflow route outcomes,
  compaction replay, event count, and 28-tool inventories match current source
  and tests; the `docs/cli.md` anchor resolves.
- [x] **A4 — TUI/package truth:** no active spec or current user guide claims a
  retained Rust TUI; plugin/sidebar and 14-test inventories include Workflow;
  checkout and installed launch commands are distinguishable; package/release
  prose names the prepared TUI runtime, Compat adapter, deterministic bundle
  writer, and updater omission accurately. `packages/hya-tui-ts/UPSTREAM.md`
  explicitly records that the installer includes/verifies `NOTICE` while the
  `0.36.7` release workflow/rehearsal does not, without changing packaging.

- [x] **A5 — Test/release/spec truth:** matrix docs include T2.14 and T0-T3,
  coverage is dated, current flake/CI behavior is explicit, active Trellis
  specs have accurate status/path references, and release guidance includes the
  exact no-publish rehearsal and current payload assertions.
- [x] **A6 — Link/path integrity:** repository-relative Markdown links and
  anchors in the edited/current documentation resolve; stale archived-task paths
  point to archive locations or are removed; nonexistent template paths are
  labeled as such; no archived file is modified.
- [x] **A7 — Cross-document consistency:** a version/command/source-claim scan
  finds no contradictory current claims across root docs, architecture pages,
  package READMEs, wiki indexes, specs, and testing/release docs. Deferred owner
  decisions remain explicitly labeled rather than asserted as facts.
- [x] **A8 — Scope safety:** implementation changes only approved documentation
  and indexes, leaves source/tests/config behavior untouched, preserves all
  immutable exclusions, reviews no-drift documents without rewriting them, and
  handles the in-scope `AGENTS.md` only through `agent-dotfile` plus
  `writing-for-agents` with managed markers and install/sync verification.
- [x] **A9 — Contract verification:** the targeted commands in `implement.md`
  pass (or an environment prerequisite is reported precisely), including version
  metadata, docs examples, matrix/P19, TUI Workflow tests, installer smoke,
  release rehearsal, actionlint, and final link/path scans.

**Execution resolution — 2026-08-31:** The stale literal assertion was replaced
by behavioral execution of the real release SDK guards; `bash
tests/install_script.sh` now passes with exit 0, and the installer test plus all
follow-up gates pass. Release behavior remained unchanged.


## Notes

- Planning is complete only after `design.md`, `implement.md`,
  `implement.jsonl`, and `check.jsonl` contain real, repository-grounded
  artifacts and `python3 ./.trellis/scripts/task.py validate
  .trellis/tasks/08-31-documentation-audit-sync` passes.
- Do not run `task.py start` or implement product/documentation edits in this
  planning turn. A later user message must explicitly approve this final
  planning summary before execution.
