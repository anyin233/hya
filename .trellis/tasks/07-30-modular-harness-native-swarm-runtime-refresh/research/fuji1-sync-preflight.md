# Fuji1 workspace and sync preflight

## Read-only checks performed

The following checks were run before creating or modifying this task:

- `git status --short --branch`
- `git rev-parse HEAD`
- `git rev-parse origin/main`
- `git rev-list --left-right --count origin/main...HEAD`
- `git worktree list --porcelain`
- `git remote -v`
- `hostname`, `uname -srmo`
- `df -hP` and `findmnt -T` for the authoritative workspace
- executable discovery for `rsync`, `rclone`, `unison`, `syncthing`, and
  `mutagen`
- read-only process/config/repository-name checks for those synchronizers

No package was installed, no synchronizer or service was started, no remote was
contacted, and no system/user service was modified.

Canonical role/host/session identities are defined once in
`browser-pro-escalation-protocol.md`. This document uses `fuji1 remote worker`
and `MacBook Air coordinator` so that “local” cannot be interpreted relative to
the session currently reading it.

## Authoritative environment

| Item | Observed result |
| --- | --- |
| Host role | `fuji1 remote worker` |
| Kernel | `Linux 5.15.0-185-generic x86_64` |
| Workspace | `/chivier-disk/yanweiye/Projects/yaca` |
| Mount | `/dev/sdf[/yanweiye/Projects/yaca]`, `ext4`, `rw,nosuid,nodev,relatime` |
| Capacity | 7.3 TiB total, 6.1 TiB used, 851 GiB available, 88% used |
| Worktrees at audit | One: the authoritative saved project only |
| Branch | `main...origin/main` |
| HEAD | `267bfc3c6c66e46fe8514e2e70657489f853b7f0` |
| Locally known `origin/main` | Same commit; ahead/behind `0/0` |
| Origin | `https://github.com/anyin233/hya.git` |
| Workspace version | `0.34.2` |

## Pre-existing dirty state

This state existed before task creation and must remain untouched:

```text
 M .trellis/tasks/07-23-remove-rust-tui/prd.md
 M .trellis/tasks/07-23-remove-rust-tui/task.json
 M crates/hya-sdk/src/reducer.rs
 M crates/hya-sdk/src/store.rs
 M crates/hya-sdk/src/types.rs
 M crates/xtask/src/startup_bench.rs
 D fixtures/agents.json
 D fixtures/display_golden.json
 D fixtures/live_session_turn.jsonl
 D fixtures/live_tool_turn.jsonl
 D fixtures/turn_stream.jsonl
 D "imgs/Hya icon v7.png"
?? .trellis/tasks/07-21-grok-build-provider/
?? .trellis/tasks/07-22-review-and-merge-open-prs/
?? .trellis/tasks/07-23-repository-root-cleanup/
?? docs/assets/8bit-examples/
?? docs/assets/hya-icon.png
?? docs/research/
?? tests/fixtures/
```

## Synchronizer inventory

- Present: `/usr/bin/rsync`.
- Not found on `PATH`: `rclone`, `unison`, `syncthing`, `mutagen`.
- No corresponding user config directory was found at the checked standard
  locations.
- No active synchronizer process was found; the process query itself was the
  only matching command line.
- Repository matches for synchronizer names were generic documentation/editor
  references, not a project-specific source-to-mirror configuration.

## Required development and mirror boundary

This is the operating rule for the authorized `0.34.3` execution:

1. Source edits, dependency resolution, code generation, builds, tests,
   benchmarks, release staging, and artifact signing run only on the
   `fuji1 remote worker`, in the single task worktree described below.
2. The `MacBook Air coordinator` is a read-mostly mirror/coordination host; it
   is not a second build authority.
3. Default repository synchronization is one-way:
   `fuji1 remote worker -> MacBook Air coordinator`.
4. A later dry run must exclude at least:
   - `.git/`
   - `target/`
   - every `node_modules/`
   - runtime SQLite databases and sidecars (`*.db`, `*.db-*`) plus runtime state
   - `.env*`, credentials, signing keys, tokens, and other secrets
   - Codex/Trellis/hya worktrees and ephemeral worktree metadata
5. Deletion propagation must remain disabled until both endpoints and the
   exclusion manifest are reviewed from a dry-run listing.
6. No bidirectional synchronizer or background service is introduced by this
   architecture task without a separate, explicit operational decision.

## Preflight conclusion

The host, repository identity, and available space are sufficient for the
authorized implementation. A safe sync command cannot yet be
constructed because no `MacBook Air coordinator` destination/transport identity
was supplied, and none is needed for this audit. The next sync-related action,
if separately authorized, is a non-deleting
`rsync --dry-run --itemize-changes` from an explicit `fuji1 remote worker`
source to an explicit `MacBook Air coordinator` destination using a reviewed
exclusion file.

## 0.34.3 isolation update

Before starting implementation:

- dirty `main` remained at
  `267bfc3c6c66e46fe8514e2e70657489f853b7f0` with the exact 19 protected
  user-owned paths above plus the original untracked task directory;
- no pre-existing task branch or worktree was found;
- branch `codex/modular-harness-native-swarm-runtime-refresh` and worktree
  `/chivier-disk/yanweiye/Projects/yaca/.worktrees/modular-harness-native-swarm-runtime-refresh`
  were created from that exact HEAD;
- the isolated tracked tree was clean before the existing task directory was
  copied;
- `diff -qr` confirmed the source and worktree task directories were identical
  immediately after copying;
- subsequent task-document changes occur only in the isolated worktree copy.

On 2026-07-31, immediately before release staging:

- a direct remote query and fetch observed `origin/main` at
  `156d0ad3c50aea67dfac0054485eb6991e77308b`, one README-only commit beyond the
  audit anchor;
- the isolated branch was cleanly rebased to that commit using a recoverable
  include-untracked stash, then the exact `0.34.3` and task changes were
  restored without conflict;
- dirty `main` was not checked out or fast-forwarded and remains at
  `267bfc3c6c66e46fe8514e2e70657489f853b7f0` with the same 19 protected paths
  plus the task directory.

This worktree is implementation isolation, not a mirror or synchronization
endpoint. It remains excluded from any future one-way mirror.
