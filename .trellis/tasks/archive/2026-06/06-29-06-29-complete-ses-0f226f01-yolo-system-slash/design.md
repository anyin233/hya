# Complete ses_0f226f01 yolo and system slash cleanup — Design

## Architecture

The change is a cleanup of command ownership, not a new command system.

- TUI-only actions stay owned by the TUI command palette and TUI controller.
- Public slash commands stay owned by backend slash-command registries and Compat metadata.
- System slash commands that are local TUI actions are intercepted before any `/session/.../command` request.
- Prompt macros and unknown slash commands are not claimed by the local system-command router.
- CLI `--yolo` remains a startup switch; only interactive `/yolo` exposure is removed.

## Contracts

### YOLO

- Internal palette label: `Switch YOLO`.
- Selection opens an Enable/Disable dialog.
- Enable path updates the TUI auto-approve/YOLO state visibly.
- `/yolo` is absent from public slash registries, help/completion, backend lookup, and Compat-compatible command metadata.
- `/yolo` is not a local system command; it should follow the same fallback path as unknown/custom commands where applicable.

### Local system slash routing

Local-only commands:

- `/think` -> local variant/reasoning selector.
- `/tools` -> local tool/MCP status surface.
- `/mcp` -> local tool/MCP status surface.
- `/?` -> local help.
- `/quit`, `/exit`, `/q` -> local quit.

Non-local commands:

- `/init` remains handled by its existing non-local path.
- Prompt macro commands such as `/review` remain prompt macros.
- Unknown slash commands, plain paths, and `/yolo` do not masquerade as local system commands.

## Surfaces

- `crates/hya-tui/src/app/runtime.rs`: command-palette action dispatch and slash input routing.
- `crates/hya-tui/src/keymap/action.rs`: internal action enum/labels.
- `crates/hya-tui/src/keymap/defaults/table.rs`: default command-palette action table.
- `crates/hya-tui/src/keymap/tests.rs`: internal palette/action regressions.
- `crates/hya-backend/src/tui/commands.rs`: builtin slash registry/help/completion behavior.
- `crates/hya-backend/src/tui/controller.rs`: TUI slash routing split.
- `crates/hya-server/src/compat/command_catalog.rs`: Compat-compatible command metadata.
- Server tests: command metadata and provider/model command regressions.
- `docs/cli.md`: CLI `--yolo` remains documented; interactive `/yolo on` is not recommended.
- `swebench/scripts/hya_drive.sh` and adjacent docs/results/findings: automation uses palette YOLO enablement.

## Testing approach

TDD for any behavior not already covered:

1. Add tests that fail if `/yolo` is still exposed in the relevant registry/metadata.
2. Add tests that fail if local system slash commands fall through.
3. Add tests that fail if prompt macros/unknown/path commands are incorrectly claimed locally.
4. Add or update tests proving `Switch YOLO` exists in the internal palette.
5. Update swebench script/docs and run shell syntax validation.

## Verification

On branch and again after merge to `main`:

- Focused command routing tests.
- `bash -n swebench/scripts/hya_drive.sh` if the script exists.
- `cargo fmt --all --check`.
- `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo test --workspace`.
- `git diff --check`.
- Scripted/manual TUI QA for palette `Switch YOLO`, YOLO enable state, `/think`, `/tools` or `/mcp`, `/quit`, and absence of public `/yolo` presentation.

## Rollback

If a branch-owned verification failure appears, fix on the branch before merge. If a failure is unrelated and reproducible from a clean main worktree without branch changes, record exact paths/errors in the Trellis progress notes and final response; do not hide it.
