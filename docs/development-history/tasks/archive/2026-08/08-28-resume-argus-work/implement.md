# Implementation Plan

## Execution Rule

The parent task owns integration and acceptance. Start and finish one child at a time. Do not start the parent because it has no direct code ownership. Every child uses this loop:

1. Read its PRD, design, implementation plan, and injected specs.
2. Add one focused failing behavior test for the next contract.
3. Run that exact test and record the expected missing-behavior failure.
4. Implement the smallest production change that passes.
5. Run the focused test again.
6. Repeat by vertical contract; do not batch unproved implementation.
7. Run the child gate from the main session.
8. Run a Trellis check pass, fix findings, and rerun affected checks.
9. Finish/archive the child only when its acceptance criteria pass.

Subagents may edit only after the main session records the red test. They skip formatters, linters, and suites; the main session owns every validation command.

## Child Order and Ownership

### 1. `08-28-recovered-argus-baseline`

Owns classification and release of the existing Argus predecessor tree only.

- Re-read the complete diff by subsystem. Preserve all user changes.
- Remove `.argus_subagents`, `.planning`, temporary logs, and runtime artifacts from commit scope without deleting untracked user data.
- Correct stale provider timeout/retry documentation and any `0.35.2` claim that does not match executable behavior.
- Verify existing provider resilience, `find` containment, Workflow foundation, Trellis runtime, and release metadata.
- Stage only recovered tracked files.
- Commit `chore(release): 0.35.2` and push after the full baseline gate.
- Rollback point: this commit is the clean base for the breaking Workflow work.

Baseline gate:

```bash
RUSTUP_TOOLCHAIN=1.91.1 cargo fmt --all --check
RUSTUP_TOOLCHAIN=1.91.1 cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=1.91.1 cargo test --workspace --exclude hya-e2e
RUSTUP_TOOLCHAIN=1.91.1 cargo build -p hya-backend --bin hya-backend
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-e2e -- --test-threads=1
(cd crates/hya-plugin-compat/adapter && bun run typecheck && bun test)
(cd packages/hya-tui-ts && bun run typecheck && bun test)
```

### 2. `08-28-workflow-language-runtime`

Owns `hya-workflow` and the core executor cutover.

TDD order:

1. New-format linear compile and exact normalized plan.
2. Fan-out/fan-in grammar, declaration order, and automatic bounded evidence.
3. Source-located invalid syntax, input, cycle, old-format, loop, and actor failures.
4. Existing transient runtime behavior through `CompiledWorkflow`.
5. Actual provider overlap for same-level fan-out.
6. Two sequential resident activations on one actor Session.
7. Same-level actor collision, resident failure, collect-all result, and cancellation boundary.
8. Migrate CLI/tool fixtures only enough to keep the core public interface compiling; control-surface behavior remains child 4.

Focused gate:

```bash
RUSTUP_TOOLCHAIN=1.91.1 cargo fmt --all --check
RUSTUP_TOOLCHAIN=1.91.1 cargo clippy -p hya-workflow -p hya-core --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-workflow
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-core --test workflow
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-core --test resident --test resident_recovery
```

Mutation gate:

- Serialize the fan-out future once; the overlap test must fail.
- Force the second actor activation to spawn; the one-Session/two-mail test must fail.
- Revert both mutations before proceeding.

### 3. `08-28-workflow-bundles-examples`

Depends on the stable `hya-workflow::compile` interface. Owns prepared format v2, WorkflowBundle, plugin contributions, installed adapter delivery, catalogs, and first-party/example sources.

TDD order:

1. AgentBundle remains one Agent while WorkflowBundle prepares one Workflow plus Agent closure.
2. Prepared-v2 canonical bytes/digests and strict public package closure.
3. Multi-Agent/Workflow catalog indexing, collision checks, and registry atomic install.
4. Plugin Skill/tool contribution equality and removal of direct bundle Skill parsing.
5. Installed Compat adapter resolution outside the repository.
6. Runtime snapshot publishes Workflow and Agents together while old bindings remain pinned.
7. First-party plan/impl/review bundle resolves without auto-selection.
8. Full Argus example packages, installs, lists, and resolves without special engine logic.

Focused gate:

```bash
RUSTUP_TOOLCHAIN=1.91.1 cargo fmt --all --check
RUSTUP_TOOLCHAIN=1.91.1 cargo clippy -p hya-workflow -p hya-bundle -p hya-plugin -p hya-store -p hya-app -p hya-backend --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-bundle
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-plugin
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-store --test bundle_registry
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-app
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-backend --test bundle_cli
(cd crates/hya-plugin-compat/adapter && bun run typecheck && bun test)
bash tests/install_script.sh
```

Mutation gate:

- Disable packaged-Agent closure validation; the missing-Agent test must fail.
- Accept a wrong Skill digest; the plugin contribution test must fail.
- Remove installed adapter fallback; the outside-workspace smoke must fail.
- Revert mutations.

### 4. `08-28-durable-workflow-control`

Depends on compiled/catalog interfaces. Owns durable types/events/projection, app control, CLI/tool/server/SDK routes, slash interception, and interrupted recovery.

TDD order:

1. Projection selection/replay preserves the exact transcript.
2. Run/stage/member events, stale-run filtering, terminal stickiness, and member-link deduplication.
3. Store close/reopen and dead-owner interruption without Stage replay.
4. App control selection, revision checks, idempotent run admission, ToolOperation retention, and typed errors.
5. CLI/tool migrate to the shared control adapter.
6. Native/legacy/v2 typed endpoints and `/workflow` pre-model dispatch.
7. SDK typed state/activity and structured non-2xx errors.
8. Real server restart and Session switch/run behavior.

Focused gate:

```bash
RUSTUP_TOOLCHAIN=1.91.1 cargo fmt --all --check
RUSTUP_TOOLCHAIN=1.91.1 cargo clippy -p hya-proto -p hya-store -p hya-core -p hya-app -p hya-server -p hya-sdk -p hya-backend --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-proto
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-store
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-core --test workflow
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-app
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-server --test workflow_session_api
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-sdk
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-backend --test workflow_cli
```

Mutation gate:

- Replace replay-derived selection with process state; the reopen test must fail.
- Bypass `/workflow` interception; the parent-provider counter test must fail.
- Drop the ToolOperation id; the duplicate-run test must fail.
- Revert mutations.

### 5. `08-28-workflow-tui-presentation`

Depends only on the typed server Session state and existing SSE invalidation. Owns the hya built-in sidebar plugin and minimal Sync typing.

TDD order:

1. Pure state parser and presentation for none/ready/running/terminal/stale states.
2. Sidebar plugin registration, order, cleanup, clipping, parallel `+N`, and semantic colors.
3. Bootstrap hydration and `session.updated` replacement with no timer or second client.
4. Counts ignore unrelated run-tree members and use server-derived Workflow activity.
5. Existing `/workflow` command path keeps prompt/transcript behavior and never requires a parent model.
6. PTY behavior at existing narrow/wide sizes and roster observation reuse.

Focused gate:

```bash
cd packages/hya-tui-ts
bun run typecheck
bun test test/workflow-presentation.test.ts test/sdk-spine.test.ts test/branding-pruning.test.ts
bun test test/pty-smoke.test.ts
```

Mutation gate:

- Count all run-tree members; the unrelated-Agent count test must fail.
- Add a polling refresh; the no-timer/request-count test must fail.
- Revert mutations.

Visual gate:

- Build the actual TUI.
- Start a real backend with the deterministic fake provider through `hub`.
- Open the TUI in a PTY, select/run the simple Workflow, and observe sidebar transitions and child roster navigation.
- Repeat at the existing narrow layout size. Record exact observed states; a source-only snapshot is not proof.

### 6. `08-28-workflow-integration-release`

Owns cross-child integration, documents, domain language, ADR, release versions, full gates, and final commit/push. It does not add new product behavior.

- Update `CONTEXT.md` with Workflow, Stage, Actor Key, WorkflowBundle, Workflow Identity, and Workflow Run only; keep implementation facts out.
- Supersede the affected author-format part of ADR-0013 and add one ADR for WorkflowBundle plus event-sourced Workflow control if the final design still meets the ADR threshold.
- Update user, architecture, bundle, plugin, server/SDK, TUI, installer, and testing documents. Remove contradictory `stages:`/`needs:` and zero-first-party-Workflow claims.
- Archive `CHANGELOG.md` as `docs/changes/CHANGELOG_0.35.2.md`; write only `0.36.0` notes at root.
- Set workspace, lockfile, and TUI package versions to `0.36.0`.
- Run cross-layer examples and all mutation criteria once more.

Final Rust gate:

```bash
RUSTUP_TOOLCHAIN=1.91.1 cargo fmt --all --check
RUSTUP_TOOLCHAIN=1.91.1 cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=1.91.1 cargo test --workspace --exclude hya-e2e
RUSTUP_TOOLCHAIN=1.91.1 cargo build -p hya-backend --bin hya-backend
RUSTUP_TOOLCHAIN=1.91.1 cargo test -p hya-e2e -- --test-threads=1
```

Final TypeScript/install gate:

```bash
(cd crates/hya-plugin-compat/adapter && bun run typecheck && bun test)
(cd packages/hya-tui-ts && bun run typecheck && bun test)
(cd packages/hya-tui-ts && bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts)
bash tests/install_script.sh
```

Final smoke:

1. Install the built artifacts into an isolated prefix.
2. Run `hya bundle list/info`, inspect the first-party Workflow, and verify no Session auto-selects it.
3. Install the Argus example package through the public CLI.
4. Run the simple and Argus Workflows with FakeLlm; observe Stage/Agent state through the server and actual TUI.
5. Restart the backend; verify selection restores and an intentionally abandoned run becomes Interrupted without duplicate child effects.
6. Switch Workflow and compare the full pre-existing message id/content vector.

After every gate passes, run the commit skill, stage only the complete `0.36.0` feature and Trellis records, commit `feat(workflow): add user-composed workflow platform`, and push. Do not tag or publish a GitHub Release.

## Final Review Checklist

- Every parent acceptance criterion maps to passing evidence.
- Every exported symbol migration has exhaustive call-site coverage.
- Old author/prepared paths, aliases, docs, fixtures, and re-exports are removed.
- New Event variants are classified in every exhaustive match.
- No second scheduler, plugin contribution path, state reducer, HTTP client, poller, or terminal renderer exists.
- No input values/full outputs appear in lifecycle Events or logs.
- No unrelated untracked/runtime artifact is staged.
- The final pushed commit matches the verified tree.
