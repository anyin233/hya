# OpenCode parity worktree PR split design

## Worktree map

| Child task | Branch | Worktree | Version |
| --- | --- | --- | --- |
| `07-03-opencode-version-release-hygiene` | `feat/opencode-version-release-hygiene` | `.worktrees/opencode-version-release-hygiene` | `0.29.3` |
| `07-03-opencode-compat-mcp-import` | `feat/opencode-compat-mcp-import` | `.worktrees/opencode-compat-mcp-import` | `0.29.4` |
| `07-03-opencode-tui-theme-picker` | `feat/opencode-tui-theme-picker` | `.worktrees/opencode-tui-theme-picker` | `0.29.5` |
| `07-03-opencode-revert-snapshot-baseline` | `feat/opencode-revert-snapshot-baseline` | `.worktrees/opencode-revert-snapshot-baseline` | `0.29.6` |

## Independence model

The business-code write sets are intentionally separated:

- Version hygiene: root README/release metadata plus a metadata regression test.
- Compat MCP import: `crates/hya-app` config import and `crates/hya` CLI import test.
- TUI theme picker: `crates/hya-legacy-tui` theme state/rendering plus `crates/hya-backend` controller commands.
- Revert snapshot baseline: `crates/hya-tool` file-change metadata and `crates/hya-server` revert restore behavior.

All child PRs must update `Cargo.toml`, `Cargo.lock`, and root `CHANGELOG.md` because of the project release rule. That creates expected metadata conflicts if PRs are merged out of order. Recommended merge order is ascending version number.

## Agent contract

Each worker owns exactly one worktree and must:

- Read `AGENTS.md`, this child task's `prd.md`, `design.md`, and `implement.md`, plus relevant `.trellis/spec/**/index.md` before editing.
- Add one failing test first and show the failing command in its final report.
- Keep production edits inside the assigned write set except for the required version/changelog files.
- Commit only its atomic change and push its branch.
- Leave PR creation to the main agent unless explicitly told otherwise in the worker prompt.

## PR ordering

Open PRs separately. Mark PRs 2-4 with a note that `Cargo.toml`/`Cargo.lock`/`CHANGELOG.md` may need a trivial version/changelog rebase if a lower-numbered parity PR merges first.
