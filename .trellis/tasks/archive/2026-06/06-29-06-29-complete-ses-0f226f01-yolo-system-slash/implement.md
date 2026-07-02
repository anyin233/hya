# Complete ses_0f226f01 yolo and system slash cleanup — Implementation Plan

> Execute inline from a new isolated worktree. Do not edit project code in the original dirty checkout.

## Goal

Complete YOLO slash-command removal/internal palette migration and local/system slash routing cleanup, then verify, merge to `main`, push, remove the worktree, and archive this Trellis task.

## Global constraints

- Source session: `ses_0f226f01affemrXG9TnsGx2NBb`.
- Worktree path: `.worktrees/complete-ses-0f226f01-yolo-system-slash`.
- Branch: `complete-ses-0f226f01-yolo-system-slash`.
- Base: current `main`/`origin/main` containing `30df233`, `d5ade75`, and `fdae2a6`.
- Do not implement in the original dirty checkout.
- Preserve unrelated dirty files from the original checkout.
- Commit directly; no user approval required.
- Keep commits semantic, atomic, one-line, and without AI attribution.
- Merge completed branch back to `main`, rerun verification on merged `main`, push `origin/main`, remove worktree, prune worktrees, then archive this task.
- CLI `--yolo` remains valid and documented; only interactive/public slash exposure is removed.

## Exact spec/context files

- `.trellis/spec/frontend/index.md`
- `.trellis/spec/backend/index.md`
- `.trellis/spec/guides/index.md`
- `.trellis/spec/guides/code-reuse-thinking-guide.md`
- `.trellis/spec/guides/cross-layer-thinking-guide.md`
- `DESIGN.md` only if TUI rendering is changed beyond tests/routing.

## Abort and recovery criteria

- If `.worktrees/` is not ignored, stop code work, add only `/.worktrees` to `.gitignore`, commit `chore: ignore local worktrees`, then create the worktree.
- If branch `complete-ses-0f226f01-yolo-system-slash` already exists, inspect it with `git worktree list` and `git rev-parse`; reuse only if it points at the required path and base, otherwise stop and record the conflict.
- If any required base commit is missing from `main` or `origin/main`, stop and record the missing commit; do not implement against the wrong base.
- If `git fetch`/pull shows upstream divergence or would overwrite dirty main-checkout files, stop before merge/push and record the divergence.
- If merge conflicts occur, resolve only conflicts in files owned by this task; preserve unrelated dirty files and rerun full verification.
- If a verification failure reproduces on clean `main` before merge, record exact command/output as unrelated; otherwise fix the branch-owned failure before proceeding.
- Rollback before merge: reset or amend the worktree branch only. Rollback after merge but before push: revert the merge on `main` or reset `main` to pre-merge only if not pushed and no unrelated local work is touched.

## Phase 1: Worktree setup

- [ ] Run `git status --short --branch` in `/chivier-disk/yanweiye/Projects/yaca`; save the dirty/untracked baseline in the final notes.
- [ ] Verify `.worktrees/` exists with a directory read or `test -d .worktrees`.
- [ ] Verify `.gitignore` contains `/.worktrees` and `git check-ignore -q .worktrees` exits 0.
- [ ] If ignore is missing, edit only `.gitignore`, run `git diff --check`, then `git add .gitignore && git commit -m "chore: ignore local worktrees"`.
- [ ] Run `git worktree add .worktrees/complete-ses-0f226f01-yolo-system-slash -b complete-ses-0f226f01-yolo-system-slash main`.
- [ ] In the worktree, run `git merge-base --is-ancestor 30df233 HEAD`, `git merge-base --is-ancestor d5ade75 HEAD`, and `git merge-base --is-ancestor fdae2a6 HEAD`.
- [ ] From this point, set `cwd` to `.worktrees/complete-ses-0f226f01-yolo-system-slash` for all code reads/edits/tests.

## Phase 2: TDD red tests and audits

- [ ] Read the exact spec/context files listed above.
- [ ] In `crates/hya-tui/src/app/runtime.rs`, inspect tests near `palette_tui_commands_include_yolo_switch_action`, `builtin_client_command_routes_builtin_slashes_to_client_actions`, `command_like_name_detects_command_syntax_not_paths`, and `builtin_quit_command_detects_documented_slash_exit_aliases`.
- [ ] Edit only `crates/hya-tui/src/app/runtime.rs` test `builtin_client_command_routes_builtin_slashes_to_client_actions` to assert `/think`, `/tools`, `/mcp`, and `/?` map to local client actions, and `/init`, `/review`, `/bogus`, `/usr/bin/x`, and `/yolo on` return `None`.
- [ ] Edit only `crates/hya-tui/src/app/runtime.rs` test `palette_tui_commands_include_yolo_switch_action` to assert title `Switch YOLO`, command `permission.yolo.switch`, category `Permission`, and enabled flag true.
- [ ] Edit only `crates/hya-tui/src/app/runtime.rs` test `builtin_quit_command_detects_documented_slash_exit_aliases` to assert `/quit`, `/exit`, and `/q` are true and `/quit now`, `/exit now`, `/q now` do not route as backend commands through any submit-path test added below.
- [ ] If no behavior-level quit submit test exists, add one `crates/hya-tui/src/app/runtime.rs` test named `slash_quit_aliases_do_not_queue_or_submit_backend_commands` that sets `backend_ready = false`, submits `/quit`, `/exit`, and `/q` through the same prompt-submit method used by Enter, and asserts no pending prompt/backend submit is created and the local exit path is selected.
- [ ] In `crates/hya-backend/src/tui/commands.rs`, inspect tests near `resolves_slash_commands_and_aliases`, `unknown_slash_command_is_not_resolved`, `help_items_come_from_registered_commands`, and `completion_items_filter_by_prefix`.
- [ ] Edit only `crates/hya-backend/src/tui/commands.rs` test `resolves_slash_commands_and_aliases` to assert `resolve_slash("yolo") == None` and `resolve_slash("init") == None` while keeping expected local commands.
- [ ] Edit only `crates/hya-backend/src/tui/commands.rs` test `help_items_come_from_registered_commands` to assert no item has label `/yolo` or `/init`.
- [ ] Edit only `crates/hya-backend/src/tui/commands.rs` test `completion_items_filter_by_prefix` to assert `/yo` and `/in` produce no built-in `/yolo` or `/init` completions.
- [ ] In `crates/hya-backend/src/tui/controller.rs`, inspect `dispatch_slash`, `is_exact_slash_command`, tests `slash_init_requests_project_initialization`, `slash_tools_opens_tool_status_dialog`, `slash_quit_requests_exit`, and `tab_toggles_yolo_when_no_popup_is_active`.
- [ ] Replace only `crates/hya-backend/src/tui/controller.rs` test `slash_init_requests_project_initialization` with `slash_init_is_not_local_builtin`, asserting `/init` is not handled as `TuiEffect::InitProject` or any local built-in effect.
- [ ] Add or edit only `crates/hya-backend/src/tui/controller.rs` tests for `/think`, `/mcp`, `/?`, `/exit`, and `/q` so each routes to its intended local effect/dialog.
- [ ] Add or edit only `crates/hya-backend/src/tui/controller.rs` custom command test so a user-defined prompt macro remains submitted and is not claimed by local built-in routing.
- [ ] Treat `tab_toggles_yolo_when_no_popup_is_active` as audit-only: do not remove Tab behavior unless it directly exposes public `/yolo` slash-command behavior or breaks a failing test tied to the PRD.
- [ ] In `crates/hya-server/tests/compat_command_metadata_api.rs`, edit `compat_command_route_includes_native_init_and_review_commands` or add one adjacent test so `/command` and `/api/command` metadata omit built-in `yolo` while preserving expected `init`/`review` metadata.
- [ ] In `crates/hya-server/tests/compat_provider_model_api.rs`, edit tests only if provider/model metadata exposes command lists; otherwise make no change and record that this file has no command metadata assertion point.
- [ ] Use `grep` for literal `/yolo`, `yolo on`, and `Switch YOLO` in `docs/cli.md`, `swebench/scripts/hya_drive.sh`, `swebench/scripts/HYA_DRIVE.md`, `swebench/RESULTS.md`, and `FINDINGS.md` before editing docs/automation.
- [ ] Run the focused test commands expected to fail before implementation:
  - `cargo test -p hya-tui builtin_client_command_routes_builtin_slashes_to_client_actions palette_tui_commands_include_yolo_switch_action builtin_quit_command_detects_documented_slash_exit_aliases`
  - `cargo test -p hya-backend resolves_slash_commands_and_aliases unknown_slash_command_is_not_resolved help_items_come_from_registered_commands completion_items_filter_by_prefix`
  - `cargo test -p hya-backend slash_tools_opens_tool_status_dialog slash_quit_requests_exit slash_init_is_not_local_builtin`
  - `cargo test -p hya-server compat_command_route_includes_native_init_and_review_commands`

## Phase 3: Minimal implementation

- [ ] In `crates/hya-tui/src/app/runtime.rs`, keep `PALETTE_TUI_COMMANDS` entry `Switch YOLO` and `DialogKind::YoloSwitch`; change only what failing tests require.
- [ ] In `crates/hya-tui/src/app/runtime.rs`, if the explicit quit submit-path test fails, route `builtin_quit_command` at the prompt submit path before backend-ready queueing and before `slash_command` backend submission.
- [ ] In `crates/hya-backend/src/tui/commands.rs`, delete stale public/local slash entries for `/yolo` if any exist; delete `/init` from `COMMANDS` if tests prove it is still a local built-in.
- [ ] In `crates/hya-backend/src/tui/controller.rs`, remove any `CommandKind::Init`/`TuiEffect::InitProject` dispatch if `/init` is still local; preserve custom prompt macro dispatch for user-defined `init` commands.
- [ ] Leave Tab YOLO toggling unchanged unless an explicit PRD-tied failing test proves it exposes public `/yolo` slash behavior; the task removes public slash exposure, not unrelated keybindings.
- [ ] In `crates/hya-backend/src/tui.rs` and `crates/hya-backend/src/tui/harness.rs`, remove `TuiEffect::InitProject` match arms only if the enum variant is deleted.
- [ ] In `crates/hya-server/src/compat/command_catalog.rs`, keep native `init` metadata if it is Compat-compatible prompt/native metadata; ensure no built-in `yolo` command is synthesized.
- [ ] Preserve CLI `--yolo` in backend CLI code and keep docs that describe startup `--yolo` usage.
- [ ] In swebench script/docs/results/findings, replace any automation/guidance that types `/yolo on` with command-palette `Switch YOLO` guidance.
- [ ] Do not add compatibility aliases or shims for `/yolo`.

## Phase 4: Branch verification and commits

- [ ] Rerun focused tests that were red in Phase 2 and verify they pass:
  - `cargo test -p hya-tui builtin_client_command_routes_builtin_slashes_to_client_actions palette_tui_commands_include_yolo_switch_action builtin_quit_command_detects_documented_slash_exit_aliases`
  - `cargo test -p hya-backend resolves_slash_commands_and_aliases unknown_slash_command_is_not_resolved help_items_come_from_registered_commands completion_items_filter_by_prefix`
  - `cargo test -p hya-backend slash_tools_opens_tool_status_dialog slash_quit_requests_exit slash_init_is_not_local_builtin`
  - `cargo test -p hya-server compat_command_route_includes_native_init_and_review_commands`
- [ ] Run `test ! -f swebench/scripts/hya_drive.sh || bash -n swebench/scripts/hya_drive.sh`.
- [ ] Run `cargo fmt --all --check`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace`.
- [ ] Run `git diff --check`.
- [ ] Run scripted/manual TUI QA from the worktree:
  - Launch with `cargo run -p hya-backend -- --help` first to verify binary flags still expose `--yolo`.
  - Launch the TUI in a terminal with the worktree binary.
  - Open command palette, verify `Switch YOLO` appears.
  - Select `Switch YOLO`, choose enable, verify visible YOLO/auto-approve state.
  - Type `/think`, verify the local variant/reasoning selector opens.
  - Type `/tools` or `/mcp`, verify local status opens.
  - Type `/quit`, verify local exit without backend command submission.
  - Verify `/yolo` is not listed in slash completion/help/metadata.
- [ ] Stage only task-owned files.
- [ ] Commit with one-line semantic messages, e.g. `fix(tui): complete yolo slash removal`, `fix(tui): route system slashes locally`, `docs: update yolo automation guidance`, or narrower messages matching actual diff.
- [ ] Run `git status --short --branch` in the worktree and require clean before merge.

## Phase 5: Merge, reverify, push

- [ ] Return to `/chivier-disk/yanweiye/Projects/yaca`.
- [ ] Run `git status --short --branch`; confirm only pre-existing unrelated dirty/untracked files remain and nothing from this task is staged there.
- [ ] Run `git fetch origin main`.
- [ ] If `main` is behind `origin/main` and no local dirty file would be overwritten, fast-forward or rebase safely; otherwise stop and record the blocker.
- [ ] Merge with `git merge complete-ses-0f226f01-yolo-system-slash`.
- [ ] Rerun required verification on merged `main`:
  - `cargo test -p hya-tui builtin_client_command_routes_builtin_slashes_to_client_actions palette_tui_commands_include_yolo_switch_action builtin_quit_command_detects_documented_slash_exit_aliases`
  - `cargo test -p hya-backend resolves_slash_commands_and_aliases unknown_slash_command_is_not_resolved help_items_come_from_registered_commands completion_items_filter_by_prefix`
  - `cargo test -p hya-backend slash_tools_opens_tool_status_dialog slash_quit_requests_exit slash_init_is_not_local_builtin`
  - `cargo test -p hya-server compat_command_route_includes_native_init_and_review_commands`
  - `test ! -f swebench/scripts/hya_drive.sh || bash -n swebench/scripts/hya_drive.sh`
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `git diff --check`
- [ ] Push with `git push origin main`.
- [ ] Verify `git rev-parse HEAD` equals `git rev-parse origin/main`.

## Phase 6: Cleanup

- [ ] Run `git worktree remove .worktrees/complete-ses-0f226f01-yolo-system-slash` from the main checkout.
- [ ] Run `git worktree prune`.
- [ ] Archive this Trellis task with `python3 ./.trellis/scripts/task.py archive 06-29-06-29-complete-ses-0f226f01-yolo-system-slash --no-commit` after verification, merge, push, and worktree cleanup.
- [ ] If the Trellis archive move is task-owned, commit it with `chore(trellis): archive yolo slash cleanup task`; do not stage unrelated `.trellis` dirty files.
- [ ] Run final `git status --short --branch` on `main`. If not clean because of pre-existing unrelated dirty files, list exact non-owned paths and state they were preserved.

## Plan review history

### Round 1 — oracle — VERDICT: FAIL

D1 PASS: goals and non-goals are concrete and falsifiable, including CLI --yolo preservation [implement.md:5-20]
D2 FAIL: phase items bundle multi-file audits and implementation, not 1-3 tool-call atomic steps -> split by behavior/surface with exact edit/test target per step [implement.md:34-57]
D3 FAIL: unnamed “relevant guide files” and cited symbols/commits are assumed rather than enumerated with fallback -> list exact guide paths and add existence/missing-commit handling [implement.md:14,34-39]
D4 FAIL: rollback is generic and “safe and non-destructive” has no abort criteria -> define recovery for missing commits, upstream divergence, merge conflicts, and unrelated failures [implement.md:82; design.md:74-76]
D5 FAIL: verification has broad “focused tests” and manual TUI QA without exact commands -> name cargo test filters and TUI launch/scripted QA steps per behavior [implement.md:61,67-74]
D6 PASS: scope stays on requested cleanup, preserves CLI --yolo, and forbids aliases/shims/speculative rewrites [implement.md:50-57]
VERDICT: FAIL

### Round 2 — oracle — VERDICT: FAIL

D1 PASS
D2 FAIL: Phase 2 bundles add/adjust tests across multiple assertions/files and conditional “if needed,” so a low-context implementer cannot execute each step in 1–3 tool calls -> split into one explicit test edit per file/behavior and remove conditional test instructions [.trellis/tasks/06-29-06-29-complete-ses-0f226f01-yolo-system-slash/implement.md:55]
D3 PASS
D4 PASS
D5 PASS
D6 FAIL: Plan directs removing or replacing Tab YOLO toggling even though PRD only removes public slash exposure and requires palette migration, not keybinding removal -> make Tab behavior an audit-only non-goal unless tests prove it violates public slash removal [.trellis/tasks/06-29-06-29-complete-ses-0f226f01-yolo-system-slash/implement.md:79]
VERDICT: FAIL

### Round 3 — oracle — VERDICT: FAIL

D1 PASS: goals and non-goals are concrete and falsifiable, including CLI --yolo preservation -> no fix [implement.md:5]
D2 PASS: Phase 2 is split by file/test targets with Tab YOLO kept audit-only, so steps are executable -> no fix [implement.md:51]
D3 PASS: spec files, symbols, and base-commit assumptions are enumerated with missing-base handling -> no fix [implement.md:22]
D4 PASS: abort, rollback, divergence, conflict, and unrelated-failure recovery criteria are explicit -> no fix [implement.md:31]
D5 FAIL: verification still filters for renamed slash_init_requests_project_initialization, allowing zero-test passes -> update Phase 2/4/5 commands to slash_init_is_not_local_builtin [implement.md:64]
D6 FAIL: category Model for Switch YOLO is unrequested and contradicts the existing internal Permission grouping -> assert title/command/enabled only or keep Permission [implement.md:56]
VERDICT: FAIL
