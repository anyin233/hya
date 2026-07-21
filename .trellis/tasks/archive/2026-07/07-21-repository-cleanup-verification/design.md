# Design: Repository Cleanup Verification

## Boundary

This task reconciles repository records only. It does not modify product code,
the installed hya configuration, or release metadata. Legitimate project
records are preserved; the exact foreign untracked directory is removed under
the user's explicit authorization.

## State Reconciliation

The initial dirty records map to one prior project work item and one foreign
residue:

1. The archived reasoning-default task and Session 9 describe completed,
   revalidated user-configuration work.
2. The untracked IR conformance directory belongs to another project. The user
   explicitly authorized deleting it without publishing its contents.

This cleanup task supplies the final lifecycle record. After checks pass it is
archived with `task.py archive --no-commit`, so staging and commit ownership
remain explicit.

## Commit Boundaries

1. Completed reasoning-default archive plus its Session 9 journal/index.
2. This cleanup task archive plus its Session 10 journal/index.

The single-commit alternative was rejected because it mixes completed external
configuration work with cleanup closure. More granular journal hunk commits
were rejected because the full workspace files are wholly attributable at each
of the two journal checkpoints. Deleting the untracked foreign directory
creates no commit.

## Safety Contracts

- Never stage `.`, `.trellis`, `.trellis/tasks`, or an archive parent.
- Before every commit, inspect cached names, the complete cached diff,
  whitespace, and credential-like literals.
- A remote advance, unexpected path, failed configuration assertion, or IR
  deletion precondition failure stops execution before push.
- Before deleting the foreign directory, require the literal resolved path,
  directory type, and an empty `git ls-files` result for that path.
- Use no reset, stash, broad clean, force push, or deletion beyond the exact
  user-authorized foreign directory.

## Verification

Product CI is not applicable because no product file changes. Relevant checks
are JSON/JSONL validity, task-state assertions, sanitized installed-config
validation, staged-diff audits, baseline path review, empty porcelain status,
and local/remote commit equality.

## Rollback

Each pre-push commit is independently revertible. The foreign directory has no
Git rollback because it is untracked; any path or tracking mismatch therefore
stops deletion. If exact staging is violated, unstage only unexpected paths
without changing the worktree. If the remote advances, keep local commits and
stop for integration review; never force push.
