# OpenCode parity PR stack design

## Stack map

| Order | Child task | Branch | Base | Version | PR | PR head commit |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `07-03-opencode-version-release-hygiene` | `feat/opencode-version-release-hygiene` | `main` | `0.33.9` | `#7` | `beb0b296` |
| 2 | `07-03-opencode-compat-mcp-import` | `feat/opencode-compat-mcp-import` | version hygiene | `0.33.10` | `#9` | `877f9b2a` |
| 3 | `07-03-opencode-tui-theme-picker` | `feat/opencode-tui-theme-picker` | Compat MCP import | `0.33.11` | `#8` | `a55ecae9` |
| 4 | `07-03-opencode-revert-snapshot-baseline` | `feat/opencode-revert-snapshot-baseline` | TUI theme picker | `0.33.12` | `#10` | `b2669312` |
| 5 | control record | `chore/opencode-parity-task-plan` | revert snapshot baseline | docs only | `#11` | this PR |

## Dependency model

The business-code write sets remain separated by feature. Release metadata is shared by project rule, so the PR bases encode a linear stack rather than leaving version/changelog conflicts for merge time. Merge bottom-up in the table order.

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
