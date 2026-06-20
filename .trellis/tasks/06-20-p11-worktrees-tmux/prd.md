# Phase 11 worktrees and tmux

> Child of parent task **06-20-agent-spec**. Authoritative spec lives in the
> parent — do NOT duplicate it here (single source of truth, avoids drift):
> - `.trellis/tasks/06-20-agent-spec/implement.md` → this phase's deliverables,
>   exact `cargo` validation command, and rollback (feature-gate + `git tag`).
> - `.trellis/tasks/06-20-agent-spec/design.md` → the architecture/contracts.
> - `.trellis/tasks/06-20-agent-spec/prd.md` → product requirements (D1–D5).

## Scope
Implement this phase exactly as specified in the parent `implement.md`. Update
the parent artifacts (not this stub) if the plan changes during execution.

## Done when
The phase's `cargo` validation gate in the parent `implement.md` is green and
`cargo clippy --workspace --all-targets -- -D warnings` passes.
