# Verify Recovered Argus Baseline

## Outcome

Turn the mixed, uncommitted Argus predecessor tree into one reviewed and pushed `0.35.2` baseline without losing or sweeping unrelated user work.

## Requirements

- Classify every tracked diff as provider resilience, OAuth recovery, model fallback, `find` containment, narrowed Workflow foundation, Trellis runtime, release metadata, or unrelated user state.
- Preserve all intentional tracked changes. Exclude runtime logs, Argus artifacts, planning scratch files, and unrelated untracked data from staging.
- Correct documentation or changelog statements that do not match the recovered executable behavior, including provider stream-idle support.
- Keep the existing `0.35.2` version alignment and newest-only root changelog contract.
- Run the complete Rust workspace, process E2E, Compat adapter, and TypeScript TUI gates on the exact staged tree.
- Commit and push only after all gates pass.

## Acceptance Criteria

- [x] `git diff --check` and release metadata checks pass for the classified tree.
- [x] Provider, OAuth/fallback, `find`, Workflow foundation, and Trellis behavior have passing focused or recovered mutation evidence.
- [x] Rust CI-equivalent gates, rebuilt backend Track P, Compat adapter checks, and TUI checks pass with zero failures.
- [x] Commit `chore(release): 0.35.2` contains only classified tracked files and is pushed to the configured upstream.
- [x] The remaining worktree contains only intentional planning/user state for the `0.36.0` Workflow cutover.

## Exclusions

- No new Workflow public contract, bundle format, durable state, or TUI feature is implemented in this child.
