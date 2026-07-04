# Complete ses_0f226f01 yolo and system slash cleanup

> **Superseded note:** ADR-0005 removed the backend legacy TUI modules referenced below. Treat `crates/hya-backend/src/tui*` paths in this archived PRD as historical context only.


## Source

- Source session: `ses_0f226f01affemrXG9TnsGx2NBb`.
- This continuation must close the full session scope, not only the previously pushed subset.
- Prior pushed base commits required on `main`/`origin/main`:
  - `30df233 feat(tui): move yolo toggle to command palette`
  - `d5ade75 docs(cli): document switch yolo palette action`
  - `fdae2a6 fix(tui): route system slash commands locally`

## Goals

### Goal A: YOLO slash-command removal and internal switch migration

Interactive YOLO must no longer be a public slash command. It must be an internal TUI command-palette/dialog switch like Switch Model.

Expected behavior:

- TUI command palette includes `Switch YOLO`.
- Selecting `Switch YOLO` opens an Enable/Disable dialog.
- Enabling YOLO visibly enables auto-approve mode in the TUI.
- Interactive `/yolo` is not a built-in/public slash command.
- `/yolo` is not resolved by backend/TUI slash registries.
- `/yolo` is not advertised in help/completion.
- Compat-compatible command metadata does not expose `yolo`.
- CLI `--yolo` remains valid and documented.
- swebench automation enables YOLO through the command palette, not by typing `/yolo on`.
- Docs do not tell users to use `/yolo on`.

### Goal B: Built-in system slash commands route locally

Built-in slash commands that are local/system TUI actions must not fall through to `/session/.../command` as skill/model-style prompts.

Expected behavior:

- `/think` routes locally to variant/reasoning selector.
- `/tools` and `/mcp` route locally to status dialog.
- `/?` routes locally to help.
- `/quit`, `/exit`, and `/q` quit locally.
- Every built-in slash command representing a local/system TUI action is intercepted locally.
- Prompt macro commands remain prompt macros.
- Unknown slash commands do not masquerade as system commands.
- `/init`, custom prompt macros, unknown commands, paths, and `/yolo` still fall through or behave intentionally.
- `/yolo` remains removed as a public slash command.
- Tests prove the command routing split.

## Required process constraints

- Use this Trellis task for the whole run.
- Use a new git worktree at `.worktrees/complete-ses-0f226f01-yolo-system-slash`.
- Work only inside that worktree after setup; do not implement in the original dirty checkout.
- Branch name: `complete-ses-0f226f01-yolo-system-slash`.
- Merge the completed branch back into `main`.
- Push merged `main` to `origin/main`.
- End with `main` clean: no staged, unstaged, or untracked files caused by this work.
- Preserve unrelated dirty files from the original checkout.
- Commit directly when coherent atomic changes are ready; no user approval required.
- Keep commits semantic, atomic, one-line, and without AI attribution.
- Prefer boring/minimal fixes; delete stale code/docs rather than adding compatibility shims.

## Surfaces to audit

- `crates/hya-tui/src/app/runtime.rs`
- `crates/hya-tui/src/keymap/action.rs`
- `crates/hya-tui/src/keymap/defaults/table.rs`
- `crates/hya-tui/src/keymap/tests.rs`
- `crates/hya-backend/src/tui/commands.rs`
- `crates/hya-backend/src/tui/controller.rs`
- `crates/hya-server/src/compat/command_catalog.rs`
- `crates/hya-server/tests/compat_command_metadata_api.rs`
- `crates/hya-server/tests/compat_provider_model_api.rs`
- `docs/cli.md`
- `swebench/scripts/hya_drive.sh`
- `swebench/scripts/HYA_DRIVE.md`
- `swebench/RESULTS.md`
- `FINDINGS.md`

## Acceptance criteria

- [ ] Trellis task captures source session, Goal A, Goal B, worktree requirement, merge-back requirement, and final clean-tree requirement.
- [ ] `.worktrees/` exists and is ignored before worktree creation; if an ignore entry is missing, a minimal setup commit adds it separately.
- [ ] Worktree branch is created from current `main`/`origin/main` and contains required prior commits `30df233`, `d5ade75`, and `fdae2a6`.
- [ ] `Switch YOLO` exists in the internal TUI command palette.
- [ ] Selecting `Switch YOLO` opens the Enable/Disable dialog.
- [ ] Enabling YOLO visibly enables auto-approve mode in the TUI.
- [ ] `builtin_client_command("/yolo") == None` or the equivalent direct routing assertion holds.
- [ ] Backend slash resolution, help, and completion omit `/yolo`.
- [ ] Compat command metadata omits `yolo`.
- [ ] CLI `--yolo` remains valid and documented.
- [ ] swebench automation uses command palette YOLO enablement and not `/yolo on`.
- [ ] Docs do not instruct users to use `/yolo on`.
- [ ] Tests prove `/think`, `/tools`, `/mcp`, `/?`, `/quit`, `/exit`, and `/q` route locally.
- [ ] Tests prove `/init`, `/review` or prompt macro examples, unknown commands, plain paths, and `/yolo` do not route as local built-ins.
- [ ] `bash -n swebench/scripts/hya_drive.sh` passes if the script exists.
- [ ] Focused command routing tests pass.
- [ ] `cargo fmt --all --check` passes.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] `cargo test --workspace` passes.
- [ ] `git diff --check` passes.
- [ ] Scripted/manual TUI QA covers command palette `Switch YOLO`, YOLO enable state, `/think`, `/tools` or `/mcp`, `/quit`, and absence of public `/yolo` presentation.
- [ ] Branch changes are committed atomically.
- [ ] Branch is merged into `main`.
- [ ] Full required verification is rerun on merged `main`.
- [ ] `main` is pushed to `origin/main`, and local `HEAD` matches upstream.
- [ ] Temporary worktree is removed and stale worktrees pruned.
- [ ] Trellis task is archived only after verification, merge, push, worktree cleanup, and final clean-tree check.
