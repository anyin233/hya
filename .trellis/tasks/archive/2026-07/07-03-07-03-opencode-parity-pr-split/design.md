# OpenCode parity PR stack design

## Stack map

| Order | Child task | Branch | Base | Version | PR | PR head commit |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `07-03-opencode-version-release-hygiene` | `feat/opencode-version-release-hygiene` | `main` | `0.33.11` | `#7` | `a8c657bd` |
| 2 | `07-03-opencode-compat-mcp-import` | `feat/opencode-compat-mcp-import` | version hygiene | `0.33.12` | `#9` | `4de8cb0f` |
| 3 | `07-03-opencode-tui-theme-picker` | `feat/opencode-tui-theme-picker` | Compat MCP import | `0.33.13` | `#8` | `1bf4aa8b` |
| 4 | `07-03-opencode-revert-snapshot-baseline` | `feat/opencode-revert-snapshot-baseline` | TUI theme picker | `0.33.14` | `#10` | `42ebbd9d` |
| 5 | control record | `chore/opencode-parity-task-plan` | revert snapshot baseline | docs only | `#11` | this PR |

## Dependency model

The business-code write sets remain separated by feature. Release metadata is shared by project rule, so the PR bases encode a linear stack rather than leaving version/changelog conflicts for merge time.

Merge bottom-up in the table order. After each PR merges, fetch `origin/main`, retarget the immediate successor PR to `main`, verify that its diff now contains only its own slice, and wait for the retarget-triggered checks to pass before merging it. Repeat through #11; an ordering note alone is not sufficient because leaving the successor based on the merged feature branch can keep it blocked or produce a stale comparison.

Each feature slice follows the same contract:

- Add one atomic failing test and confirm the expected missing behavior.
- Implement the smallest bounded change that satisfies the child PRD.
- Update the sequential project version and archive the previous root changelog.
- Run `cargo fmt --all --check`, strict workspace Clippy, all workspace tests, and local `hya`/`hya-backend` builds.
- Commit and push only after the gate passes.

## Scope boundaries

- Version hygiene owns cross-file release metadata validation.
- Compat MCP import owns config parsing, mapping, merge behavior, CLI evidence, and documentation.
- Theme picker owns the current `hya-tui` built-in command and dialog flow without persistence or redesign.
- Revert snapshots own edit-event metadata and filesystem restoration for edit snapshots, not a complete patch stack or history pruning system.
- The control PR owns Trellis evidence and PR topology only.
