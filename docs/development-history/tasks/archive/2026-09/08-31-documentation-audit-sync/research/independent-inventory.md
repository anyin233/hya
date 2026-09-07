# Independent Documentation Inventory

## Summary

Completed a read-only independent audit. No files were created or changed. I inspected 681 tracked Markdown files, removed 342 explicit historical/vendor exclusions, classified 339 remaining files, and audited 92 high-priority current documents. A local relative-link scan covered those 92 documents.

## Inventory

| Group | Count | Files |
| --- | ---: | --- |
| Root contributor and index documents | 6 | `AGENTS.md`, `README.md`, `CHANGELOG.md`, `CONTEXT.md`, `DESIGN.md`, `CLAUDE.md` |
| Trellis project specifications | 19 | 8 backend specs, 8 frontend specs, 3 guides under `.trellis/spec/` |
| Trellis workflow | 1 | `.trellis/workflow.md` |
| Testing documentation | 4 | `docs/testing/README.md`, `agent-matrix.md`, `coverage-baseline.md`, `process-e2e.md`; also inspected the superseded `ci-agent-e2e-snippet.yml` |
| Crate/package README and provenance documents | 6 | `crates/hya-e2e/README.md`, `crates/hya-plugin-compat/README.md`, `packages/hya-tui-ts/{README.md,UPSTREAM.md,scripts/README.md,test/README.md}` |
| Current docs tree | 51 | `docs/README.md`; 14 ADRs; 10 architecture pages; agent, CLI, configuration, development, install, testing, TUI, workflow, bundle/plugin, self-update, troubleshooting, and comparison pages |
| Wiki/index mirrors | 5 | `.autors/hya/wiki/{INDEX.md,README.md,pages/**}` |
| Read-only source/config evidence | 0 | `Cargo.toml`, `install.sh`, `scripts/package-argus-example.sh`, `.github/workflows/{ci,release}.yml`, `crates/xtask`, `crates/hya-e2e/matrix.toml` and P19, hya-tool registry, workflow routing source/tests, TUI test tree |
| Intentionally outside rewrite scope | 247 | 99 active-task artifacts, 5 workspace journals, 96 managed agent/skill mirrors, 43 prompt/fixture documents, and 4 historical plans/research pages |

## Stale Claims

### P0 — Retained Rust TUI references

**Location:** `.trellis/spec/frontend/directory-structure.md:18-19`; `component-guidelines.md:53`; `quality-guidelines.md:32,61`

**Claim:** These specs still refer to retained Rust TUI crates or a retained Rust renderer.

**Evidence:** `crates/hya-tui` and `crates/hya-tui-lib` do not exist. `docs/adr/0010-remove-retained-rust-tui.md:3-14` records their deletion. `AGENTS.md:105-107` and `docs/architecture/tui.md:439-446` state that `packages/hya-tui-ts` is the only interactive frontend.

**Action:** Delete the retained-Rust wording. State the clean TypeScript-only boundary.

### P0 — Canonical Tool count and inventory

**Location:** `docs/architecture/agent-tool-surface.md:24-35`; `docs/architecture/tools-and-permissions.md:16-52`; `docs/compat-parity.md:84`

**Claim:** These pages say `ToolRegistry::builtins` installs 26 canonical tools. Their inventories omit the `workflow` tool; `tools-and-permissions` also omits `announce`.

**Evidence:** `crates/hya-tool/src/tool.rs:325-361` registers `WorkflowTool` and `AnnounceTool`. hya-core count assertions require 28. `docs/testing/agent-matrix.md:68-75` already has the correct 28-tool inventory.

**Action:** Change 26 to 28 and add `workflow` and `announce` in the affected tables. Add `workflow` to `docs/project-structure.md:114-136` as well.

### P0 — Missing T2.14 matrix row

**Location:** `docs/testing/agent-matrix.md:52-64`

**Claim:** The implemented Track P table stops at T2.13 and omits T2.14.

**Evidence:** `crates/hya-e2e/matrix.toml:283-289` registers T2.14 at `p19_workflow_model_routing.rs`. P19:83-258 proves explicit fallback, separate worker/verifier efforts, bounded route outcomes, and close/reopen replay. The current registry has 36 Track P IDs across 19 files and 4 Track T IDs.

**Action:** Add the T2.14 row with the P19 oracle.

### P0 — Missing xtask commands

**Location:** `AGENTS.md:91`; `docs/development.md:101-123`; `docs/project-structure.md:28`

**Claim:** The xtask inventory lists only `sync-compat`, `migrate`, `startup-bench`, and `matrix-check`.

**Evidence:** `crates/xtask/src/main.rs:30-40` also dispatches `package-bundle` and `release-rehearsal`.

**Action:** Document both commands, their purpose, and their real `cargo run -p xtask --` invocation.

### P0 — Release verification contract is incomplete

**Location:** `.trellis/spec/backend/quality-guidelines.md:478-486`

**Claim:** The release verification contract covers three binaries and the TUI runtime, but does not name the current Compat adapter and Argus WorkflowBundle payloads. It says to run `actionlint` but gives no executable rehearsal command or pinned tool requirement.

**Evidence:** `.github/workflows/release.yml:108-129` packages `lib/hya/compat-adapter` and `examples/hya-argus-example.hyabundle`; lines 161-188 and 244-248 smoke them. `crates/xtask/src/release_rehearsal.rs:18-26` pins `actionlint` 1.7.12, Bun 1.3.14, the adapter, and Argus helper; lines 93-119 run the full no-publish rehearsal.

**Action:** Expand the package/smoke contract and add the exact `release-rehearsal` command. State that CI does not currently run `actionlint`; `actionlint` is enforced only when the rehearsal command runs.

### P0 — Installer and archive payload omit Compat adapter in prose

**Location:** `README.md:40-42`; `docs/getting-started.md:53-68`; `docs/architecture/tui.md:448-458`

**Claim:** Installer/archive payload descriptions name the three binaries and TypeScript runtime but omit `lib/hya/compat-adapter`.

**Evidence:** `install.sh:22-27,190-223,268-271` installs and verifies the adapter. `release.yml:108-126` includes the same production adapter.

**Action:** Add the adapter payload, locked production install, rollback, and verification rows. In `docs/architecture/tui.md`, include `lib/hya/compat-adapter/` in the archive tree.

### P1 — Compat adapter resolution precedence

**Location:** `docs/configuration.md:706,1074-1075`

**Claim:** The default Compat adapter location is described only as `crates/hya-plugin-compat/adapter`.

**Evidence:** `crates/hya-app/src/plugins.rs:80-112` resolves `HYA_COMPAT_ADAPTER_DIR` first, then executable-adjacent `../lib/hya/compat-adapter`, then the workspace path.

**Action:** Document the full precedence order.

### P1 — TUI test count and inventory

**Location:** `packages/hya-tui-ts/test/README.md:3,36-50,110-120`; `docs/development.md:68-83`

**Claim:** The package test README says there are eleven Bun test files and both inventories omit the three Workflow suites.

**Evidence:** The tracked test directory contains 14 `*.test.ts` files, including `workflow-presentation.test.ts`, `workflow-sidebar.test.ts`, and `workflow-pty.test.ts`.

**Action:** Change the count to 14. Classify the three Workflow suites and their backend/PTY requirements in both indexes.

### P1 — Testing index range

**Location:** `docs/testing/README.md:19`

**Claim:** The matrix page is called a Tier 0-2 inventory.

**Evidence:** `matrix.toml:291-320` and `agent-matrix.md:93-105` include T3.1-T3.4.

**Action:** Use scenario IDs T0-T3, not Tier 0-2.

### P1 — Coverage baseline called current

**Location:** `docs/testing/README.md:59-61`

**Claim:** It calls 85.56% the current workspace coverage figure.

**Evidence:** `coverage-baseline.md:9-12` pins that number to 2026-08-05 commit `1a7db256`. The repository is now 0.36.7 with later code and tests.

**Action:** Call it the dated baseline measurement. Do not claim it is current without a fresh coverage run.

### P1 — hya-workflow missing from component maps

**Location:** `AGENTS.md:67-92`; `docs/project-structure.md:31-55`; `docs/architecture/overview.md:28-37`

**Claim:** The component/crate maps omit `hya-workflow`. The first two maps also omit its ownership boundary entirely.

**Evidence:** `Cargo.toml:22-23` includes `hya-workflow`. `crates/hya-workflow/src/lib.rs:1-20` owns Workflow source compilation, immutable normalized plans, revisions, and rendering inputs.

**Action:** Add `hya-workflow` as the sole Workflow authoring/compiler boundary. Keep runtime execution in `hya-core` and composition/control in `hya-app`.

### P1 — Raw 7z package authoring commands

**Location:** `docs/agent-bundle-authoring.md:60-70,382-386`

**Claim:** Authoring examples still prescribe raw `7z a` commands and do not mention the canonical deterministic package writer.

**Evidence:** `crates/xtask/src/package_bundle.rs:1-38` validates a source directory and atomically emits canonical public package bytes. `scripts/package-argus-example.sh:13-17` calls it as the single package-format authority. `tests/install_script.sh:28-33` rejects 7z creation in the helper.

**Action:** Prefer `cargo run -p xtask -- package-bundle <source-dir> <output.hyabundle>`. If raw 7z remains, label it as a noncanonical manual alternative.

### P1 — Backend spec index status

**Location:** `.trellis/spec/backend/index.md:9,15-23`

**Claim:** The index says to fill every file and marks database, error, and quality guidelines as `To fill` although these files contain project-specific contracts.

**Evidence:** `database-guidelines.md`, `error-handling.md`, and the 800+ line `quality-guidelines.md` are substantive current project specs.

**Action:** Correct statuses and remove generic bootstrap wording where the guide is already populated.

### P2 — NOTICE missing from release payload

**Location:** `packages/hya-tui-ts/UPSTREAM.md:31-33` versus `release.yml:117-126` and `release_rehearsal.rs:793-797`

**Claim:** `UPSTREAM` tells packaged readers to see `NOTICE`, but the release workflow/rehearsal does not copy `NOTICE`. `install.sh` does copy it.

**Evidence:** `install.sh:208-211` and 261-271 include and verify `NOTICE`. `release.yml` and rehearsal copy only `LICENSE` and `UPSTREAM.md`.

**Action:** Choose one package contract: preferably include `NOTICE` in release/rehearsal, or stop making the release payload refer to an absent file. This is an adjacent package-code decision, not only prose.

### P2 — Unavailable Cargo alias in usage strings

**Location:** `crates/xtask/src/main.rs:39`; `package_bundle.rs:11`; `release_rehearsal.rs:125-126`

**Claim:** User-facing usage strings say `cargo xtask` even though this workspace has no Cargo alias.

**Evidence:** `docs/development.md:103-110` correctly says the working form is `cargo run -p xtask -- ...`.

**Action:** Keep docs on the working form. Treat the source usage strings as a small adjacent code fix outside a documentation-only patch.

## Broken Links and Paths

### Relative-link scan

All relative path targets in the 92-document primary scope exist. No missing Markdown target file was found.

### Broken anchor

**Location:** `docs/cli.md:336`

**Target:** `#db-empty-string-semantics`

**Evidence:** The heading is `docs/cli.md:32`, `` `--db` empty-string semantics ``. The GitHub slug retains the leading hyphens.

**Action:** Use `#--db-empty-string-semantics` or rename/add a stable heading anchor.

### Missing or stale code-span paths

| Location | Documented path | Actual state/action |
| --- | --- | --- |
| `.trellis/spec/backend/workflow-control.md:107` | `.trellis/tasks/08-28-durable-workflow-control/implement.md` | Actual: `.trellis/tasks/archive/2026-08/08-28-durable-workflow-control/implement.md` |
| `docs/testing/coverage-baseline.md:72` and `.github/workflows/ci.yml:67` | `.trellis/tasks/08-05-land-swarm-branch-to-main/findings.md` | Actual: `.trellis/tasks/archive/2026-08/08-05-land-swarm-branch-to-main/findings.md` |
| `Cargo.toml:2` | `.trellis/tasks/06-20-agent-spec/{design.md,implement.md}` | Actual: `.trellis/tasks/archive/2026-06/06-20-agent-spec/{design.md,implement.md}` |
| `.trellis/workflow.md:708` | `.trellis/spec/cli/backend/workflow-state-contract.md` | No such file exists anywhere in the repository. |
| `.trellis/workflow.md:709` | `.trellis/scripts/inject-workflow-state.py` | No such file exists. Current implementations are platform-specific, for example `.codex/hooks/inject-workflow-state.py` and `.omp/extensions/trellis/index.ts`. |
| `.trellis/spec/guides/code-reuse-thinking-guide.md:217-223` | `packages/cli/src/templates/trellis/scripts/` and `.trellis/scripts/packages/cli/...` | These are upstream Trellis-template paths, not hya repository paths. Label the section as upstream/template-only or exclude it from project-local instructions. |
| `.trellis/spec/backend/task-tool.md:302-304` | `tests/support/` | Ambiguous at repository root. The referenced shared helper appears to be `crates/hya-app/tests/support/`; make the crate path explicit. |

## Likely Current Documents Needing No Edits

| Documents | Reason |
| --- | --- |
| `docs/workflows.md:164-224`, `.autors/hya/wiki/pages/architecture/workflow-composition.md:55-104`, `docs/architecture/providers.md:147-164` | They agree on suffix-free model IDs, separate per-candidate reasoning, safe pre-stream-only fallback, request-local routes, bounded `WorkflowStageRouteOutcome` data, and no TUI route rendering. `crates/hya-bundle/tests/docs_example.rs:324-353` pins these markers. |
| `.trellis/spec/backend/workflow-control.md:1-105` | The functional control/routing/replay contract matches current source and tests. Only its focused-gate path at line 107 is stale. |
| `.trellis/spec/frontend/workflow-presentation.md` | It correctly keeps route fields in typed APIs while preserving compact TypeScript TUI presentation, and names the three Workflow test classes. |
| `crates/hya-e2e/tests/p19_workflow_model_routing.rs` and `docs/testing/process-e2e.md:14` | P19 is present and the harness guide already names p19 and its focused command. |
| `docs/testing/agent-matrix.md:68-75` | The 28 canonical tool count and 15/13 Track P split are current. Only the missing T2.14 row needs change. |
| `docs/testing/coverage-baseline.md` numeric tables and fixed 18/27 measurement | These are dated, commit-pinned historical measurements. Keep the measured numbers; only fix the missing archived-task path and stop calling the number current from the testing index. |
| `docs/architecture/tui.md:3-20,388-446`; `docs/adr/0010-remove-retained-rust-tui.md`; `AGENTS.md:51-74,105-107` | These correctly describe the `hya -> hya-ts -> TypeScript/OpenTUI -> backend` chain and no Rust TUI. Only the install payload list and component omissions need edits. |
| `docs/cli.md` except line 336 | Backend/global flags, hya launcher flags, Workflow commands, revision alias, input behavior, auth commands, and exit-code descriptions match the current clap sources. |
| `README.md:19-30`, `packages/hya-tui-ts/package.json`, `CHANGELOG.md`, `crates/hya/tests/version_metadata.rs` | All report version 0.36.7; `Cargo.lock` workspace packages also match. |
| `packages/hya-tui-ts/README.md` and `scripts/README.md` | The frontend-only `--url` requirement, strict flag list, Bun 1.3.14 pin, SDK prune contract, and callers match source. The separate `test/README` count is stale. |
| `crates/hya-e2e/README.md` and `crates/hya-plugin-compat/README.md` | They are concise, link to the authoritative guides, and match current harness/adapter pins and boundaries. |
| `docs/testing/ci-agent-e2e-snippet.yml` | It is clearly marked `SUPERSEDED` and accurately points to the current CI Track P/T gates. |

## Recommended Scope Boundaries

### Rewrite now

- Patch exact current-contract drift in `AGENTS.md`, `.trellis/spec`, `docs/testing`, tool inventories, installation/release docs, component maps, package authoring commands, and the one broken CLI anchor.
- Keep changes surgical. Do not rewrite working Workflow routing prose that is already contract-tested.
- Update current documentation indexes when a child page changes.

### Exclude

- `.argus_subagents/**`, `.trellis/tasks/archive/**`, active `.trellis` task artifacts owned by the primary planner, evidence files, `docs/changes/CHANGELOG_*.md`, vendor/generated/node_modules/target, workspace journals, managed skill/platform mirrors, and agent prompt sources.
- Do not refresh dated coverage numbers unless a new coverage measurement is deliberately run.
- Do not rewrite ADR historical rationale. Only repair current path/link references when needed.
- Keep source usage-string and release `NOTICE` discrepancies as explicit adjacent code/package decisions unless the task scope is expanded.

## Recommended Verification Commands

### Version contract

```sh
cargo test -p hya --test version_metadata
```

### Workflow documentation markers

```sh
cargo test -p hya-bundle --test docs_example
```

### Matrix registry and P19

```sh
cargo run -p xtask -- matrix-check && \
  cargo test -p hya-e2e --test p19_workflow_model_routing -- --test-threads=1
```

### TUI Workflow docs/test inventory

```sh
cd packages/hya-tui-ts && \
  bun run typecheck && \
  bun test test/workflow-presentation.test.ts test/workflow-sidebar.test.ts
```

### Workflow PTY contract

```sh
cargo build -p hya-backend --bin hya-backend && \
  cargo build -p hya-ts --bin hya-ts && \
  cd packages/hya-tui-ts && \
  bun test test/workflow-pty.test.ts
```

### Installer/package contract

```sh
bash -n install.sh scripts/package-argus-example.sh tests/install_script.sh && \
  bash tests/install_script.sh
```

### Release rehearsal unit contracts

```sh
cargo test -p xtask --test release_rehearsal
```

### Full non-publishing release contract

Prerequisite: `actionlint` 1.7.12 and Bun 1.3.14 must be on `PATH`.

```sh
cargo run -p xtask -- release-rehearsal \
  --workflow .github/workflows/release.yml \
  --version 0.36.7 \
  --target x86_64-unknown-linux-gnu \
  --no-publish
```

### Workflow YAML lint only

```sh
actionlint .github/workflows/ci.yml .github/workflows/release.yml
```

### Final textual sanity

```sh
git diff --check
```

## Verification Performed

Read-only evidence only: tracked-file classification, targeted source/test/config reads, matrix parsing, and a local relative-path/heading-anchor scan. No build, test, formatter, or file-writing command was run.
