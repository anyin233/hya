# Agent status rendering implementation plan

## Preconditions

- Confirm the task is activated only after this plan is approved.
- Preserve all unrelated Trellis workspace, task, and archive changes.
- Confirm `0.33.15` is the next unused patch version before editing release metadata.

## Ordered Checklist

1. Add a failing legacy prompt regression in `crates/hya-server/tests/compat_session_legacy_message_model_api.rs`.
   - Submit an object-form model with nested variant `low` and top-level variant `high`.
   - Assert both the response user message and loaded session model retain `high`.
   - Run `cargo test -p hya-server --test compat_session_legacy_message_model_api` and record the expected RED result.

2. Preserve the prompt variant at the legacy boundary.
   - Add `variant: Option<String>` to the existing payload.
   - Apply a trimmed non-empty value only to object-form model input before the existing parser.
   - Run the focused server test to GREEN.

3. Change the existing terminal-pane tests to assert retention.
   - Cover terminal completion for focused and unfocused observations.
   - Cover a later focus change without pane removal.
   - Keep explicit close and stale-session reconciliation assertions unchanged.
   - Run `bun test test/subagent-workspace.test.ts` from `packages/hya-tui-ts` and record the expected RED result.

4. Delete terminal-driven observation cleanup.
   - Remove `closeOnBlur`, the `terminal` action and reducer branch, all three terminal dispatch sites, and terminal ID derivation used only by them.
   - Run the focused workspace test to GREEN.

5. Add failing lifecycle-presentation tests in `subagent-workspace.test.ts`.
   - Prove transient member status overrides roster `idle`.
   - Cover `Working`, `Finished`, `Failed`, `Cancelled`, and true `Idle`, including each working flag.
   - Run the focused test and record RED.

6. Implement and wire the shared lifecycle resolver.
   - Add the typed member-first resolver in `subagent-workspace.ts`.
   - Use it in the observation header and `DialogSubagent`.
   - Render the existing spinner only when the resolver reports working, including the dialog option gutter.
   - Run the focused test, `bun run typecheck`, and `bun run build` to GREEN.

7. Update required patch-release metadata.
   - Archive the current root changelog as `docs/changes/CHANGELOG_0.33.14.md`.
   - Write a root changelog containing only the new version's fixes.
   - Update `Cargo.toml`, generated `Cargo.lock` workspace entries, `packages/hya-tui-ts/package.json`, and `README.md` to the same version.

8. Run final verification.

```sh
# packages/hya-tui-ts
bun test
bun run typecheck
bun run build

# repository root
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p hya
```

9. Review `git diff` and `git status`, run Trellis quality review, then commit and push only the verified task files and release metadata.

## Review Gates

- No synthetic reasoning parts or secondary lifecycle/message store.
- No terminal cleanup path remains except explicit close and stale-session reconciliation.
- String-form model behavior and nested-only variants remain compatible.
- Status text is visible without relying on spinner or color.
- Unrelated dirty files remain unstaged and unchanged.

## Rollback Points

- Each RED/GREEN pair is independently reversible before the version update.
- If a full workspace gate fails outside touched behavior, record the exact baseline or blocker and do not commit or push.
- No database or persisted-event rollback is required.
