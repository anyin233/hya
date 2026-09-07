# Documentation synchronization implementation plan

## Execution rules

- Work only after a later user message explicitly approves this planning summary;
  do not run `task.py start` as part of planning.
- Read `prd.md`, `design.md`, `findings.md`, and
  `research/independent-inventory.md` before the first edit. Use one writer per
  document tree and keep each batch reviewable.
- Edit only current documentation/index files listed below. `AGENTS.md` is in
  scope, but its B5b edit must use `agent-dotfile` plus `writing-for-agents`.
  Before editing `.trellis/workflow.md`, load and follow `trellis-meta`; before
  editing any `.trellis/spec/**`, load `trellis-update-spec` and the applicable
  spec workflow (including `trellis-before-dev` when package/layer context is
  needed). Do not edit Rust, TypeScript, tests, generated assets, release/CI
  behavior, source usage strings, package payload behavior, archived tasks,
  historical changelogs, historical plans, dated coverage tables,
  `.argus_subagents/**`, vendor/generated trees, or workspace journals.
- For every wording change, use the claim/evidence/scope/action structure in
  `design.md`. Prefer stable symbols and test names over volatile line numbers.
- Review current no-drift documents and leave them unchanged; do not rewrite
  them merely for activity. Update an index only when a child path or link
  actually changes.
- Run `git diff --check` after each batch. Do not use a destructive reset to
  undo unrelated user work.


## Ordered checklist

### 0. Freeze evidence and scope

- [x] Read all planning artifacts and reconcile any new finding into
  `.trellis/tasks/08-31-documentation-audit-sync/findings.md` before editing
  product documentation.
- [x] Build a denylist from the immutable/out-of-scope paths in `prd.md`.
- [x] Record the pre-edit list of changed paths. Existing user changes must stay
  untouched; this task currently owns only its new task directory.
- [x] Confirm deferred decisions are labeled in prose rather than guessed:
  license payload, version ownership, model-id qualification, provider/retry
  detail, Compat scope, queued prompts/T3 gates, toolchain wiki, and xtask
  usage strings. Record the NOTICE difference as an explicit current fact in
  B3, not as a deferred wording choice.

### 1. Root, glossary, and index batch (R2)

Update only affected sections of:

- `README.md`
- `DESIGN.md`
- `CONTEXT.md`
- `CHANGELOG.md`
- `docs/README.md`
- `docs/development.md`
- `docs/project-structure.md`

Tasks:

- [x] Separate the latest public `v0.35.1` release from checked-out `0.36.7`
  source/main status; include the shipped Compat adapter without claiming an
  unsupported package or legal policy.
- [x] Describe the `hya` shim -> `hya-ts` -> backend -> TypeScript TUI chain and
  add `hya-workflow`, Workflow execution/control, bundle, native transport, and
  client boundaries to maps and source-entry links.
- [x] Correct prompt queue/overlay behavior, semantic border guidance, category
  registry ownership, Stage route terminology, 28-tool inventory, and xtask
  command forms.
- [x] Keep root `CHANGELOG.md` newest-only and clarify that explicit Stage or
  verifier assignments, not every Stage, activate ordered model routes.
- [x] Update directly affected documentation indexes only; do not copy canonical
  route or Workflow prose wholesale.

### 2. Runtime/API/architecture batch (R3)

Update confirmed drift in:

- `docs/architecture/overview.md`
- `docs/architecture/event-model.md`
- `docs/architecture/runtime.md`
- `docs/architecture/providers.md`
- `docs/architecture/agent-tool-surface.md`
- `docs/architecture/tools-and-permissions.md`
- `docs/architecture/server-client.md` only if a link/index needs repair; keep
  its route table canonical
- `docs/cli.md`
- `docs/configuration.md`
- `docs/workflows.md` only for objective wording, not its tested route prose
- `docs/compat-parity.md`
- `docs/plugin-protocol.md`

Tasks:

- [x] Add the missing `hya-workflow`/`hya-app`/`hya-bundle`/`hya-native` roles,
  Workflow/context event variants (54 total), durable `ContextCompacted`
  checkpoint semantics, and `WorkflowStageRouteOutcome` replay behavior.
- [x] Change canonical tool counts/inventories to 28 and add `workflow` and
  `announce`; repair old `tool.rs` anchors.
- [x] Explain request-local explicit route assignment, pre-stream-only fallback,
  same-route HTTP retry versus engine cross-model fallback, and provider kind
  coverage. Do not publish unapproved numeric retry constants.
- [x] Fix the `hya` synopsis from `hya [PROJECT] [OPTIONS]` to the observed
  `[OPTIONS] [PROJECT] [COMMAND]` shape, add `/workflow` to the eight-command
  catalog, and repair `docs/cli.md:336` to a resolvable anchor.
- [x] Document `HYA_COMPAT_ADAPTER_DIR` precedence: env override, executable-
  adjacent installed adapter, then workspace checkout.
- [x] Keep `docs/compat-parity.md` Compat-only, correct compaction/tool/Skill
  rows, and cross-link native Workflow docs instead of adding native rows.

### 3. TUI, package, and user workflow batch (R4)

Update confirmed drift in:

- `docs/architecture/tui.md`
- `docs/tui-reference.md`
- `docs/tui-keybindings.md`
- `docs/getting-started.md`
- `docs/agent-bundle-authoring.md`
- `docs/examples/self-update/README.md`
- `docs/adr/0005-drop-legacy-tui-surface.md`
- `docs/adr/0006-tui-session-reset-and-subagent-visibility.md`
- `packages/hya-tui-ts/README.md`
- `packages/hya-tui-ts/UPSTREAM.md`
- `packages/hya-tui-ts/test/README.md`


Tasks:

- [x] Replace active retained-Rust-TUI wording with the TypeScript/OpenTUI-only
  shipped boundary; preserve ADR historical rationale.
- [x] Correct the builtin plugin inventory to 12 declared/11 default, put
  `internal:sidebar-workflow` first, and document Workflow sidebar state.
- [x] Add `/model` and `/think` aliases and local-slash precedence. Mark the
  `session.queued_prompts` key as an accepted but unwired definition unless a
  separate product task restores its handler.
- [x] Change the package test inventory to 14 and classify the three Workflow
  test suites, including backend/PTY requirements.
- [x] Use `./target/debug/hya .` for uninstalled checkout examples and reserve
  bare `hya` for installed layouts. Remove the nonexistent monorepo-root Bun
  workspace claim.
- [x] Describe the prepared runtime subset, `lib/hya/compat-adapter`, installer
  verification, and standard release omission of `hya-updater` accurately.
- [x] In `packages/hya-tui-ts/UPSTREAM.md` and any directly affected package
  prose, state the observed NOTICE difference explicitly: `install.sh` includes
  and verifies `NOTICE`, but the `0.36.7` release workflow/rehearsal does not.
  This documents current behavior and does not change packaging.

- [x] Make `cargo run -p xtask -- package-bundle ...` the canonical authoring
  command. If raw `7z` remains as an educational fallback, label it
  noncanonical.
- [x] Correct self-update stage/report/discard/activate instructions and label
  feature-introduction versus checked-out versions.

### 4. Testing, release, and active spec batch (R5)

Before editing `.trellis/workflow.md`, load and follow `trellis-meta`. Before
editing any `.trellis/spec/**` file, load `trellis-update-spec` and the
applicable spec workflow (including `trellis-before-dev` when package/layer
context is needed). These gates protect generated/managed boundaries and keep
code-spec updates executable and source-backed.

Update confirmed drift in:

- `docs/testing/README.md`
- `docs/testing/agent-matrix.md`
- `docs/testing/coverage-baseline.md` only its current/index wording and stale
  path references; preserve measured tables
- `docs/testing/process-e2e.md` only if cross-links require it
- `.trellis/spec/backend/index.md`
- `.trellis/spec/backend/quality-guidelines.md`
- `.trellis/spec/backend/workflow-control.md`
- `.trellis/spec/backend/task-tool.md`
- `.trellis/spec/frontend/directory-structure.md`
- `.trellis/spec/frontend/component-guidelines.md`
- `.trellis/spec/frontend/quality-guidelines.md`
- `.trellis/workflow.md`
- `.trellis/spec/guides/code-reuse-thinking-guide.md`

Tasks:

- [x] Add T2.14/P19 to the matrix, use T0-T3 wording, label 85.56% as the
  2026-08-05 dated baseline, and update current flake evidence to stable
  symbols/statuses.
- [x] Document CI's `--jobs 1` cap and distinguish package smoke from non-gating
  PTY tests; do not add a new product gate.
- [x] Expand active release quality guidance to include Compat adapter and Argus
  bundle payloads, exact `release-rehearsal --no-publish` invocation, and pinned
  actionlint/Bun prerequisites. Do not alter release workflow behavior.
- [x] Correct backend index statuses where substantive guides exist. Remove
  generic bootstrap text only from populated guides.
- [x] Remove nonexistent retained Rust TUI claims from active frontend specs.
- [x] Repair archived-task references to actual archive paths, and make the
  `crates/hya-app/tests/support/` path explicit.
- [x] Mark upstream Trellis template paths in the code-reuse guide as template-
  only or remove them from project-local instructions. Replace nonexistent
  workflow-state contract/parser references with actual platform ownership.

### 5. Wiki, README, and agent-instruction boundary (R6/R8)

#### B5a. Current wiki and package README review

- [x] Audit `.autors/hya/wiki/{INDEX.md,README.md,pages/**/*.md}` and all crate /
  package README files against the ledger.
- [x] Review every current document and leave no-drift documents unchanged;
  edit only confirmed contradictions and directly affected indexes.
- [x] Leave machine-specific
  `.autors/hya/wiki/pages/environment/rust-toolchain.md` unchanged unless its
  Agent owner approves a wording update. If approved, label its date/scope
  instead of asserting a current toolchain policy.

#### B5b. `AGENTS.md` current-document update

- [x] Treat `AGENTS.md` as in scope because this request covers all current
  project documents. Before editing, load both `agent-dotfile` and
  `writing-for-agents`; use the dotfile workflow to distinguish repository-root
  instructions from installed `agents/*` dotfiles.
- [x] Preserve the managed Trellis block byte-for-byte and edit only the current
  component/verification guidance identified in findings. Do not modify
  unrelated agent mirrors.
- [x] Run the required install/sync verification: dry-run install, scenarios,
  shell syntax checks, and secret checks. Record its result with the final
  documentation verification.


### 6. Cross-document reconciliation

- [x] Re-read every changed document and compare terminology, counts, version
  labels, native/Compat boundaries, and links against `findings.md`.
- [x] Review every audited no-drift document and confirm its content remains
  unchanged; explain any index-only update by its child-path/link change.
- [x] Run `git diff --check` and inspect `git diff --name-only` against the
  allowlist. Confirm no immutable or code path changed and that `AGENTS.md` was
  edited only through B5b's required workflows.
- [x] Check every index points to the current path and no page claims an
  excluded historical document is current guidance.
- [x] Resolve only objective findings. Keep every deferred owner decision
  explicitly labeled in the relevant page and in the final report.

## Verification commands

Run these after implementation, from the repository root unless a working
directory is shown. The planning turn does not run them.

### Required focused contracts

```sh
cargo test -p hya --test version_metadata
cargo test -p hya-bundle --test docs_example
cargo run -p xtask -- matrix-check
cargo test -p hya-e2e --test p19_workflow_model_routing -- --test-threads=1
```

```sh
cd packages/hya-tui-ts
bun run typecheck
bun test test/workflow-presentation.test.ts test/workflow-sidebar.test.ts
```

```sh
cargo build -p hya-backend --bin hya-backend
cargo build -p hya-ts --bin hya-ts
cd packages/hya-tui-ts
bun test test/workflow-pty.test.ts
```

```sh
bash -n install.sh scripts/package-argus-example.sh tests/install_script.sh
bash tests/install_script.sh
cargo test -p xtask --test release_rehearsal
```

### Release and command/help checks

The full rehearsal requires `actionlint` 1.7.12 and Bun 1.3.14 on `PATH`:

```sh
cargo run -p xtask -- release-rehearsal \
  --workflow .github/workflows/release.yml \
  --version 0.36.7 \
  --target x86_64-unknown-linux-gnu \
  --no-publish
actionlint .github/workflows/ci.yml .github/workflows/release.yml
cargo run -p hya -- --help
cargo run -p hya-backend -- --help
cargo run -p hya-backend -- workflow --help
cargo run -p xtask -- --help
```

Compare help output with the edited CLI synopsis and command catalog. Do not
change source usage strings merely to make a documentation check pass.

### Link, version, command, and path scans

Run a repository-relative Markdown link/heading-anchor scan over the edited
current scope, excluding `docs/changes/CHANGELOG_*.md`, archived tasks,
vendor/generated trees, and historical plans. Also scan code spans that claim
repository paths; all current paths must resolve or be explicitly marked
historical/template-only. Confirm `docs/cli.md`'s `--db` anchor resolves.

Run a version consistency scan over current docs and package metadata. It must
show `0.36.7` for the Rust workspace/TUI/release examples where applicable,
separate the latest public `v0.35.1`, and preserve intentionally independent
adapter `0.0.0`/TUI `local` values. Do not flag dated historical versions as
current drift.

Run a command scan that compares documented `cargo run -p xtask -- ...`, launcher
synopses, slash commands, aliases, and test commands with the observed help,
source registration, and CI/release commands. Run a source-claim scan for the
28 canonical tools, 54 Event variants, 12/11 plugin inventory, 14 TUI test files,
T2.14/P19, and adapter payload. The `docs_example`, matrix, TUI, installer, and
release tests are the executable contract checks for these claims.

## Execution result — 2026-08-31

- `bash tests/install_script.sh` **PASS** (exit 0). The stale literal assertion
  was replaced by behavioral execution of the real release SDK guards; release
  behavior remained unchanged.
- Shell per-file syntax checks **PASS**.
- Rust fmt, workspace clippy, workspace tests (**1,581 passed, 4 ignored**),
  backend build, `actionlint`, and the full diff check **PASS**.
- The existing earlier A9 checks were already all green; the sole blocker is
  cleared. A1-A9 are verified.


## Review gates and rollback points

1. **After B1:** root/index wording is internally consistent; stop if version,
   runtime chain, or changelog policy conflicts remain.
2. **After B2:** route/tool/event/Compat pages agree with canonical sources; stop
   if native and Compat behavior is conflated.
3. **After B3:** TUI/package instructions are executable for both checkout and
   installed layouts; stop if a release payload is overstated.
4. **After B4:** matrix/spec/release links and statuses resolve; stop if a dated
   measurement is rewritten.
5. **After B5:** wiki/package/AGENTS ownership is respected; stop if managed or
   historical content was touched.
6. **Final:** all acceptance criteria in `prd.md` are checked, focused commands
   pass or report exact environment blockers, and `git diff --check` plus the
   allowlist/denylist audit is clean.

If a gate fails, revert only the affected documentation batch using its narrow
review diff and repair the ledger. Never reset the worktree or modify unrelated
user changes. The release `NOTICE` discrepancy, source `cargo xtask` usage
strings, license policy, version synchronization, provider numeric retry policy,
compiler qualification, queued-prompt handler, and matrix-gating decisions are
follow-up points, not rollback reasons or invitations to change product code.

## Completion handoff

- [x] Re-read `prd.md` top to bottom and confirm no blocking open question or
  temporary brainstorm section remains.
- [x] Run `python3 ./.trellis/scripts/task.py validate
  .trellis/tasks/08-31-documentation-audit-sync` and preserve its output in the
  session report.
- [x] Do not run `task.py start` in this planning pass. Implementation begins
  only after explicit approval of the final planning summary.
