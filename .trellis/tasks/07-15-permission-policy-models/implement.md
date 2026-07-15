# Implementation Plan

## 1. Finish Planning

- [x] Create and research the Trellis task with user consent.
- [x] Trace config, registry, model-tool, direct-shell, MCP, plugin, subagent,
      ask, server, TUI, and headless permission paths.
- [x] Merge all four required planner outputs and record reconciled decisions.
- [x] Resolve exact native `AllowAlways` scope and the default read-only set.
- [x] Write `prd.md`, `design.md`, and this execution plan.
- [x] Curate real `implement.jsonl` and `check.jsonl` entries.
- [x] Validate the task artifacts.
- [x] Obtain user approval.
- [x] Run `task.py start` only after approval.

## 2. Add The Invocation Evaluator

- [x] Add the smallest `hya-tool` unit test that exercises all four models,
      ordered regex matching, shell tool/command subjects, local-read/task
      fallbacks, network-read fallback, and invalid regex compilation.
- [x] Run the focused test and record the expected RED caused by the missing
      invocation-policy types/behavior.
- [x] Add model, target, rule, exact-subject, compiled-policy, and evaluator
      types in `hya-tool::permission` without changing wildcard
      `PermissionRules` semantics.
- [x] Rerun the focused evaluator test to GREEN.

## 3. Compose Native And Legacy Permissions

- [x] Add focused RED tests proving native `AllowAlways` remembers only one
      exact subject, an effective deny overrides it, and legacy
      `AllowAlways` remains action-wide.
- [x] Add exact-vs-legacy remember scope to `AskRequest`, exact native grants,
      and `PermissionPlane::authorize` returning a call-scoped plane.
- [x] Keep `assert` precedence as danger, snapshot deny/allow, call grant except
      external directory, then existing legacy remember/interceptor/ask flow.
- [x] Update pending server coalescing/saved metadata so exact replies affect
      only identical native requests while legacy requests retain current
      action-wide behavior.
- [x] Extend exhaustive generic tool action/resource mappings in the server,
      Rust TUI, and plugin permission bridge.
- [x] Rerun focused `hya-tool`, `hya-server`, `hya-plugin`, and `hya-tui` tests
      to GREEN.

## 4. Classify Registered Tools

- [x] Add a registry RED test for canonical alias resolution and the exact
      `ReadOnly`, `Task`, `Tool`, `Command`, and `Mcp` classifications.
- [x] Store permission metadata beside registry entries; keep existing `get`
      behavior and add only the resolution API needed by dispatch.
- [x] Classify the documented local read-only set centrally, keep web tools and
      plugin tools standard, mark task, shell/bash, and MCP explicitly.
- [x] Rerun the focused registry, MCP-manager, and plugin-tool tests to GREEN.

## 5. Authorize Both Execution Paths

- [x] Extend existing model-tool and direct-shell tests with a RED assertion for
      lookup-before-ask, post-hook command matching, session/message/call
      correlation, and one prompt despite an internal action assertion.
- [x] In `engine/turn.rs`, resolve the registered tool, build its invocation
      from post-hook input, scope the permission plane to the tool call,
      authorize once, and pass the returned plane through `ToolCtx`.
- [x] Apply the same authorization order in `engine/shell.rs` without creating a
      second execution framework.
- [x] Preserve after-hook behavior and the rule that permission errors cannot be
      rewritten.
- [x] Rerun the focused core tool-round, hook, error-payload, and direct-shell
      tests to GREEN.

## 6. Carry Policy From YAML To Runtime

- [x] Add config RED tests for the documented YAML, omission defaults,
      permission-only offline config, invalid regex/enum errors, and strict
      fallback after a config error.
- [x] Add the permission DTO to `FileConfig`, compile it during `config::load`,
      and carry the compiled policy through `ResolvedConfig`, `RuntimeConfig`,
      and every `build_session_engine` call.
- [x] Make `--yolo` replace the effective model with `danger` before engine
      construction; keep the existing warning text/risk signal.
- [x] Add a headless RED test proving an unresolved ask is rejected rather than
      silently allowed, then replace scoped auto-approval for `exec`, RPC, and
      goal mode with the minimum reject responder.
- [x] Keep interactive TUI and server asks on their existing endpoints.
- [x] Update starter YAML and rerun focused `hya-app` and `hya-backend` tests to
      GREEN.

## 7. Document And Version The Feature

- [x] Update `docs/configuration.md` with the YAML schema, regex semantics,
      model matrix, defaults, headless behavior, and `--yolo` override.
- [x] Update `docs/architecture/tools-and-permissions.md` with invocation-vs-
      resource composition and exact native remember behavior.
- [x] Bump `[workspace.package].version` from `0.33.7` to `0.33.8`.
- [x] Move the current root changelog to
      `docs/changes/CHANGELOG_0.33.7.md` and write a single-version
      `CHANGELOG.md` for `0.33.8`.

## 8. Verify And Finish

- [x] Run all affected focused tests first and resolve failures without
      broadening scope.
- [x] Run `cargo fmt --all --check`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo build --workspace --bins` to produce local executables.
- [x] If the Compat adapter changes, also run `bun run typecheck` and `bun test`
      from `crates/hya-plugin-compat/adapter`; otherwise do not touch it.
- [x] Verify Cargo version, root changelog heading, and release-note archive all
      agree on `0.33.8`.
- [x] Run Trellis quality/spec review, capture any durable permission contract
      in `.trellis/spec/`, and validate the task.
- [x] Review `git diff` and `git status`; preserve all pre-existing unrelated
      changes and stage only this feature's files.
- [ ] After every gate passes, create and push one atomic semantic feature
      commit, then finish/archive the Trellis task.

## Rollback Point

Before the final commit, rollback consists only of this task's source, docs,
version, changelog, and task artifacts. No database or persisted policy migration
is introduced. If a verification gate cannot pass, do not commit or push; keep
the task active and report the exact failing command.
