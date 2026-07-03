# Implementation plan

1. Create and verify the parent/child Trellis task graph.
2. Write PRD/design/implement artifacts for each child task.
3. Create four worktrees from `main` with the branch/worktree names in `design.md`.
4. Start each child task in Trellis and set its branch/base metadata.
5. Spawn one worker agent per child task with the child task path, worktree path, assigned version, write set, and verification gate.
6. While workers run, monitor completion, review diffs, run targeted verification in each worktree, and request fixes from the owning worker where needed.
7. For each verified worktree, ensure one atomic commit, push the branch, and open a separate PR against `main`.
8. Report PR URLs, verification status, and merge-order notes.

Global final gate before reporting all done:

```sh
git status --short
git worktree list
```
