# Design

## Boundary

The cleanup uses two explicit classes:

1. Repository-owned work is reviewed, verified, staged by exact path or hunk, committed, and pushed.
2. Never-tracked machine-local planning state is preserved on disk and covered by an explicit `.gitignore` rule.

No stash, reset, checkout discard, force-add, force push, amend, or broad staging command is used.

## Atomic Groups

| Commit | Content |
| --- | --- |
| `chore: ignore local planning records` | `.gitignore`; preserve `.planning/**` locally. |
| `chore(trellis): update runtime to 0.6.7` | Trellis version, hashes, workflow/scripts, and hash-verified `.omp/**` templates. |
| `chore(agents): configure project workflows` | `CLAUDE.md`, `opencode.json`, `docs/agents/**`, archived `0.32.1` release notes, and the related `07-13` planning task. |
| `docs: record project architecture decisions` | `CONTEXT.md`, ADRs 0006-0009, and the OpenCode feature inventory after correcting verified stale statements. |
| `chore(task): record GPT 5.6 subagent E2E` | `.trellis/tasks/07-14-e2e-gpt56-sol-subagents/**`, sanitized for publishable evidence. |
| `chore(task): record GPT 5.6 subagent gap analysis` | `.trellis/tasks/07-14-close-gpt56-subagent-e2e-gaps/**`, sanitized for publishable evidence. |
| `chore(deps): remove unused workspace dependencies` | Dependency-only hunks from root/lock manifests, affected crate manifests, and the native rustdoc link. |
| `fix(tui): restore main-agent running indicator` | Authoritative `session.status` projection, exact rendered-frame tests, OpenCode row text, `0.33.10` metadata, and archived `0.33.9` notes. |
| `chore(task): archive commit-and-clean-worktree` | This task's final validated archive record. |
| `chore: record journal` | Only if Trellis finish-work updates the tracked developer journal. |

## Mixed Files

`Cargo.toml` and `Cargo.lock` contain dependency cleanup and release-version changes. The dependency commit is staged from a zero-context patch with exact `0.33.9 -> 0.33.10` hunks removed. Its staged index is exported and verified independently. After that commit, the remaining Cargo diff contains only the `0.33.10` release version changes and belongs to the TUI fix.

The earlier optimistic `TimelineRender::working` change is removed. Submitted-prompt history is not lifecycle state and can outlive an authoritative idle event. A focused RED/GREEN screen test will establish that idle status wins even when pending prompt history exists.

## Planner Disagreements

- Some proposals recommended publishing or relocating `.planning/**`; the user chose local preservation under an ignore rule because the records have no tracking precedent and contain machine-local details.
- The aggressive proposal deleted incomplete task scaffolds; rejected because cleanup must preserve existing work. The `07-13` task remains truthfully in planning state.
- Plans differed on documentation granularity. One architecture-doc commit is chosen because these files jointly establish the current project context and ADR baseline; the independently owned E2E task records remain separate commits.
- Plans differed on optimistic running state. Source tracing showed it can contradict authoritative idle status, so it is removed and covered by a regression before commit.

## Safety And Recovery

- Fetch before staging and again before push. Stop on upstream drift or non-fast-forward; never force.
- Inspect staged names, patch, stat, and `git diff --cached --check` before every commit.
- Scan staged additions for credential/private-key patterns without printing matched values.
- Undo staging mistakes only with `git restore --staged -- <exact paths>`; working files remain untouched.
- Hooks are not bypassed. Any hook-created changes are classified before continuing.
- Release publication and the `v0.33.10` tag are not part of this push.
