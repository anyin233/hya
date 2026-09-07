# Integrate and Release Workflow Platform

## Outcome

Close all cross-child contracts, document the final domain and architecture, verify the real product end to end, and push one aligned `0.36.0` Workflow feature commit.

## Requirements

- Resolve every cross-crate call site and remove the old author/prepared/control/state paths, fixtures, re-exports, and contradictory claims.
- Update `CONTEXT.md`, ADRs, user/architecture/TUI/bundle/plugin/server/testing docs, and first-party example guidance.
- Archive `0.35.2` release notes and keep only `0.36.0` in root `CHANGELOG.md`.
- Align Cargo workspace/lock and TypeScript TUI package versions at `0.36.0`.
- Run every focused gate, mutation criterion, workspace CI-equivalent gate, Track P, Track T, installer, executable smoke, restart recovery, transcript-preservation scenario, and actual TUI visual check.
- Run final Trellis review, stage only intentional feature/task files, commit, and push. Do not tag or publish a release.

## Acceptance Criteria

- [x] Every parent PRD acceptance criterion has concrete passing evidence.
- [x] No second scheduler, plugin path, read model, HTTP client, poller, or terminal renderer exists.
- [x] All changed-contract tests, mutation tests, Rust/Bun/install/process gates, and smoke scenarios pass on the final tree.
- [x] Domain terms and ADRs match the final code; documentation contains no old-format or zero-first-party-Workflow contradiction.
- [x] Versions and newest-only changelog agree on `0.36.0`.
- [x] `feat(workflow): add user-composed workflow platform` is pushed and the remaining worktree contains only intentional user state.

## Exclusions

- No release tag, GitHub Release, automatic Workflow selection, or in-flight DAG replay after process death.
