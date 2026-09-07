# Repository root cleanup

## Goal

Make the repository root intentional and navigable without changing shipped
behavior. Move tracked development assets to their owning workspace locations,
remove every registered non-main Git worktree, and reset Cargo artifacts so only
newly-built `debug` and `release` profiles remain.

## Requirements

- Reorganize tracked root paths without deleting live source, test data, or
  developer tooling:
  - move `fixtures/` to the shared workspace test-support location
    `tests/fixtures/`;
  - move the `xtask` crate under `crates/` while preserving its package name and
    `cargo run -p xtask` interface;
  - move the checked-in branding image from `imgs/` into documentation-owned
    assets and update every live consumer.
- Update live code, manifests, documentation, and generated-asset provenance so
  no active reference uses the old paths. Historical Trellis/archive records
  are evidence, not migration targets.
- Preserve intentional root-level project entry points and metadata, including
  `crates/`, `packages/`, `docs/`, `scripts/`, `tests/`, `.github/`, `.hya/`,
  `.omp/`, `.trellis/`, `README.md`, `AGENTS.md`, `CLAUDE.md`, `CONTEXT.md`,
  `DESIGN.md`, Cargo manifests, and release/install configuration.
- Enumerate every Git-registered worktree before deletion, retain the main
  worktree only, and remove each other registered worktree with Git's worktree
  command. The user explicitly authorized this deletion, including dirty
  non-main worktrees.
- Clean Cargo output with Cargo, then create one current workspace `debug`
  build and one current workspace `release` build. Do not delete unrelated
  local tool state such as `.claude/`, `.codex/`, `.opencode/`, `.omo/`,
  `.planning/`, or `.codegraph/`.
- This is a layout-only refactor; do not change public behavior, dependencies,
  package names, or release version/changelog content.

## Acceptance Criteria

- [ ] The tracked repository root contains none of `fixtures/`, `xtask/`, or
  `imgs/`, and their contents have the approved destinations.
- [ ] Every live source, CI, documentation, and generator reference resolves
  to the new path; a tracked-file scan finds no unintended old-path reference.
- [ ] `xtask` remains a workspace member and `cargo run -p xtask --
  sync-compat --help` succeeds.
- [ ] `git worktree list --porcelain` reports only the main worktree, and no
  stale registered worktree metadata remains.
- [ ] `target/` has been recreated from scratch and contains current `debug`
  and `release` profile outputs only.
- [ ] Rust formatting, linting, tests, workspace debug/release builds, TUI
  typecheck/tests, asset generation, and root shell regression tests pass.
- [ ] The final diff contains only the approved layout, task artifacts, and
  necessary reference updates; unrelated pre-existing untracked Trellis tasks
  remain untouched.

## Notes

- User approval was received on 2026-07-23 after a read-only scope audit.
- `target/` and `.worktrees/` are destructive cleanup targets only to the
  degree explicitly described above; broad `rm -rf` and `git clean` are out of
  scope.
