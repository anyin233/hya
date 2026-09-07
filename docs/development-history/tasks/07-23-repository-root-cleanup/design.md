# Repository root cleanup design

## Boundary

The cleanup applies to tracked layout plus two explicitly authorized local
artifact classes: non-main registered Git worktrees and Cargo's `target/`.
It does not alter other ignored tool state or archival project records.

## Canonical layout

| Current path | Canonical path | Reason |
| --- | --- | --- |
| `fixtures/` | `tests/fixtures/` | The data is shared by `hya-sdk` and retained `hya-tui` tests. |
| `xtask/` | `crates/xtask/` | Developer tooling is a Rust workspace crate and belongs with other crates. |
| `imgs/Hya icon v7.png` | `docs/assets/hya-icon.png` | It is a reusable branding/documentation source consumed by the README and a documented generator. |

`Cargo.toml` will keep the `crates/*` workspace glob and remove the redundant
root `xtask` member. The package remains named `xtask`, so package-level Cargo
commands do not change.

## Reference migration

Fixture consumers retain their existing relative-path strategy and change only
the root segment from `fixtures` to `tests/fixtures`. The image generator and
its provenance text are updated before regenerating the checked-in outputs.
Documentation changes are restricted to live user/developer documentation and
the root agent instructions; archive records retain historical paths.

## Destructive cleanup controls

1. Record Git's registered worktree list, branch, commit, and status before
   removal. Keep only the worktree whose path equals this repository root.
2. Remove each other registered worktree via `git worktree remove --force`,
   then run `git worktree prune`. If `.worktrees/` is empty afterward, remove
   only that empty directory.
3. Use `cargo clean` rather than raw recursive deletion for `target/`.
4. After all verification, recreate target output with workspace debug and
   release builds. This ensures the retained profile artifacts are current,
   while avoiding stale custom-target or test build trees.

Each filesystem relocation is performed with Git-aware renames and can be
reversed as an isolated diff. The worktree and Cargo-output cleanup are
irreversible by design and are separately recorded in task progress.

## Non-goals

- Changing public commands, package names, dependencies, product behavior, or
  release version.
- Deleting `.claude/`, `.codex/`, `.opencode/`, `.omo/`, `.planning/`, or
  `.codegraph/`.
- Rewriting archived task documents solely to remove old path spellings.
