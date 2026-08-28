# Implementation Plan

1. Read the parent recovery findings and current tracked diff. Record a path-level ownership table before staging.
2. Compare every current release/document claim with the recovered Argus test artifacts and implementation. Correct only contradictions.
3. Run `git diff --check`, focused provider/tool/Workflow checks, then the full baseline gate in the parent plan.
4. If a gate fails, fix the owning predecessor contract and rerun the focused failure before the full gate.
5. Run a Trellis check review against backend/frontend specs.
6. Confirm `Cargo.toml`, `Cargo.lock`, TUI package version, root changelog, and archived `0.35.1` notes agree on `0.35.2`.
7. Use the commit skill. Stage only classified tracked files, inspect the staged file list/diff, commit `chore(release): 0.35.2`, and push.
8. Record commit and gate evidence in the task, then archive this child. Do not start Workflow cutover work until the pushed baseline exists.

Rollback: do not reset or delete user files. If verification fails, leave the tree uncommitted. After push, the baseline commit is the rollback point for `0.36.0`.

## Recovered Path Ownership

| Classification | Owned paths |
| --- | --- |
| Trellis runtime update | `.agents/skills/trellis-meta/references/local-architecture/bundled-skills.md`, `.omp/skills/trellis-meta/references/local-architecture/bundled-skills.md`, `.codex/agents/trellis-{check,implement,research}.toml`, `.codex/hooks/inject-{subagent-context,workflow-state}.py`, `.trellis/.template-hashes.json`, `.trellis/.version`, `.trellis/scripts/common/{active_task,git,session_context,task_context}.py` |
| Provider resilience and OAuth recovery | `crates/hya-provider/{Cargo.toml,src/http.rs,src/http/stream.rs,src/lib.rs,src/router.rs,tests/conformance.rs,tests/http_headers.rs}`, `crates/hya-app/src/oauth/{ensure.rs,mod.rs}`, the auth wiring in `crates/hya-app/src/{config.rs,runtime.rs}`, `.trellis/spec/backend/quality-guidelines.md`, and `docs/architecture/providers.md` |
| Cross-model fallback | The fallback portions of `crates/hya-core/src/{engine.rs,engine/turn.rs}`, `crates/hya-core/tests/model_fallback.rs`, and category wiring in `crates/hya-app/src/runtime.rs` |
| `find` containment | The `FindTool` portion of `crates/hya-tool/src/tool.rs` and its regressions in `crates/hya-tool/tests/tool.rs` |
| Narrowed Workflow foundation | `crates/hya-core/{Cargo.toml,src/lib.rs,src/workflow/**,tests/workflow.rs}`, Workflow portions of `crates/hya-core/src/{engine.rs,engine/shell.rs,engine/turn.rs}`, `crates/hya-app/src/{config.rs,runtime.rs}`, `crates/hya-backend/src/{cli_args.rs,main.rs,workflow_cmd.rs}`, `crates/hya-backend/tests/workflow_cli.rs`, `crates/hya-tool/src/{lib.rs,tool.rs,workflow_plane.rs}`, all one-line `ToolCtx.workflows` fixture migrations under `crates/hya-tool/tests/**` plus the Workflow tool tests, `crates/hya-mcp/src/{bridge.rs,manager.rs}`, `crates/hya-plugin/tests/{plugin_tools.rs,respawn_declaration_drift.rs}`, `crates/hya-e2e/src/scenario.rs`, `crates/hya-e2e/tests/p17_workflow_composition.rs`, `CONTEXT.md`, `README.md`, `docs/{FOLLOWUPS.md,README.md,workflows.md}`, and `docs/adr/0013-user-assembled-agent-workflows.md` |
| Workflow shutdown code spec | `.trellis/spec/backend/task-tool.md` |
| Release metadata | Version/dependency portions of `Cargo.toml` and `Cargo.lock`, `packages/hya-tui-ts/package.json`, `CHANGELOG.md`, `docs/changes/CHANGELOG_0.35.1.md`, and `crates/hya/tests/version_metadata.rs` |
| Baseline task evidence | `.trellis/tasks/08-26-workflow-composition/**` and `.trellis/tasks/08-28-recovered-argus-baseline/**` |
| Excluded preserved state | `.argus_subagents/**`, `.autors/**`, `.planning/**`, and the parent plus five future `08-28-*` Workflow cutover task directories remain unstaged for `0.36.0`; no file is deleted or reset. |

Mixed paths are intentional and listed in every owning row. The baseline commit stages each mixed file once after all owning contracts pass.

## Verification Evidence

- Focused provider/OAuth/model-fallback/Workflow/tool checks: 66 provider tests, 20 OAuth-filtered tests, 19 core fallback/Workflow tests, five glob/grep tests, the exact `find` containment regression, four Workflow CLI tests, and P17 all passed.
- Workflow verifier cancellation regression: timed out before cancellation reached in-flight provider work, then passed after the shared Turn boundary and verifier token propagation were fixed.
- Workflow supervisor shutdown regression: passed on the cooperative implementation; replacing its in-flight `select!` with a plain `.await` reproduced the two-second shutdown timeout; restoring the implementation passed.
- Final Rust gate: formatting and workspace clippy passed; `cargo test --workspace --exclude hya-e2e` passed 1,445 tests across 243 suites with five ignored; rebuilt `hya-backend`; Track P passed 32 tests across 19 suites.
- Compat adapter: `bun run typecheck && bun test` passed 64 tests.
- TypeScript TUI: final `bun run typecheck && bun test` passed 50 tests. One preceding full run hit the existing PTY root-draft timeout; the exact PTY test then passed in isolation and the complete final rerun passed.
- Release metadata test and both staged/unstaged `git diff --check` passed.
