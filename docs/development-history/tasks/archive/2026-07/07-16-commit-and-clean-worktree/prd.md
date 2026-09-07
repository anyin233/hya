# Commit and clean shared worktree

## Goal

Commit and push all intended changes currently present in the shared `main` worktree, leaving the local worktree clean without losing, discarding, or hiding any existing work.

## Background

- The worktree began with 49 modified or untracked paths from several independently developed areas.
- The just-verified main-agent running-indicator fix is one known atomic change.
- Other changes must be classified from repository evidence before staging.

## Requirements

- Preserve every existing tracked and untracked change unless repository evidence classifies it as local-only generated/session state.
- Preserve `.planning/**` on disk under a repository ignore rule; do not publish its machine-local session details.
- Group changes into coherent atomic commits with one-line semantic commit messages matching repository history.
- Stage explicit path sets only; never use a blanket stage command for mixed changes.
- Run the relevant verification gate before committing each code-bearing group, reusing already-recorded passing evidence only where no file changed afterward.
- Push normally to the configured upstream; do not force-push or alter Git configuration.
- Do not expose secrets from local configuration or generated agent files.

## Acceptance Criteria

- [x] Every initial dirty path is classified and assigned to an atomic commit or an explicit repository ignore policy for preserved local-only state.
- [ ] All intended changes are committed with no unrelated paths mixed into an atomic commit.
- [x] Required verification passes for every code-bearing commit.
- [ ] The local branch is pushed successfully to its configured upstream without force.
- [ ] `git status --short` is empty after the push.
- [x] No pre-existing work is discarded, reverted, or left only in a stash.

## Out Of Scope

- Rewriting existing commits or force-pushing history.
- Refactoring the dirty changes beyond fixes required to verify and commit them.
- Deleting unknown files solely to make the worktree appear clean.

## Notes

- User explicitly approved creation of this Trellis task.
- User reviewed the converged plan and approved execution.
