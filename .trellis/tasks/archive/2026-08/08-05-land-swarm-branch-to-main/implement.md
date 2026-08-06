# Implement — Land swarm runtime branch into main

Execution plan. Every step lists its command and its check. Steps 4 and 8 are
review gates: stop and report, do not roll through them.

`SCRATCH` below means the session scratchpad directory.

## Step 1 — Re-verify the premises

Facts were measured at planning time; confirm nothing moved before acting.

```sh
git rev-list --left-right --count main...codex/modular-harness-native-swarm-runtime-refresh   # expect: 0	75
git rev-list --left-right --count main...origin/main                                          # expect: 0	0
git rev-parse main                                                                            # expect: 156d0ad3…
git status --porcelain
```

**Check:** all four match the plan. If `main...codex/…` is not `0	<n>`, the
fast-forward premise is dead — stop and re-plan.

## Step 2 — Back up everything that will be discarded or overwritten

```sh
B="$SCRATCH/pre-merge-backup"; mkdir -p "$B"
git diff --binary > "$B/main-dirty-binary.patch"   # --binary is REQUIRED, see note
git diff          > "$B/main-dirty.patch"          # human-readable companion
git status --porcelain > "$B/status.txt"
git rev-parse main     > "$B/main-tip-before-merge.txt"
git stash list         > "$B/stash-list.txt"
git worktree list      > "$B/worktree-list.txt"
cp -a .trellis/tasks/07-30-modular-harness-native-swarm-runtime-refresh "$B/07-30-local"
git ls-files -o --exclude-standard -z | tar --null -czf "$B/untracked-all.tar.gz" --files-from=-
```

Two corrections to the original plan, both found while executing this step:

- **`git diff` alone is not a sufficient backup.** It omits binary content, so
  the deleted `imgs/Hya icon v7.png` appears only as "Binary files differ" and
  could not be restored from it (`grep -c "GIT binary patch"` → 0). `--binary`
  fixes this. Independently, all *tracked* deletions remain recoverable via
  `git checkout HEAD -- <path>`, so there was never real data risk here — but
  the backup claim was wrong as written.
- **`git diff` never captures untracked files.** The `untracked-all.tar.gz`
  line covers them; without it the only protected untracked path would have
  been the `07-30` directory.

**Check:** `main-dirty-binary.patch` contains a `GIT binary patch` section;
`git apply --check --reverse "$B/main-dirty-binary.patch"` succeeds;
`07-30-local/` contains `prd.md`, `design.md`, `implement.md`, `task.json`,
both `.jsonl` files, and `research/`; the tar lists every `??` path from
`status.txt`.

## Step 3 — Clear the three colliding paths

Only these three. Every other dirty path stays exactly as it is.

```sh
git checkout -- crates/hya-tool/Cargo.toml crates/xtask/src/startup_bench.rs
rm -rf .trellis/tasks/07-30-modular-harness-native-swarm-runtime-refresh
git status --porcelain
```

**Check:** the two files no longer appear as modified; the `07-30` directory no
longer appears as untracked; `crates/hya-sdk/`, `.trellis/tasks/07-23-remove-rust-tui/`,
the `fixtures/` and `imgs/` deletions, and the other untracked dirs are all
still listed. Rationale for discarding rather than stashing is in `design.md`
(identical content / pure fmt reflow); the backup from step 2 is the safety net.

## Step 4 — REVIEW GATE: confirm before touching the `main` ref

Report to the user: premises re-verified, backups taken, collisions cleared,
and that the next command moves `main` forward by 75 commits. Merging is still
local and fully reversible at this point; pushing (step 8) is not.

## Step 5 — Fast-forward

```sh
git merge --ff-only codex/modular-harness-native-swarm-runtime-refresh
```

**Check:**

```sh
git rev-list --left-right --count main...codex/modular-harness-native-swarm-runtime-refresh   # expect: 0	0
git log --oneline -1                                                                          # expect: 3c07de55
ls crates/hya-e2e crates/hya-bundle crates/hya-updater docs/testing                            # all present
```

No merge commit may appear. If git created one, the premise was violated —
reset to `156d0ad3` and re-plan.

## Step 6 — Reconcile the `07-30` task artifacts

The merge restored the branch's tracked versions. Compare against the local
copies backed up in step 2.

```sh
diff -ru "$SCRATCH/pre-merge-backup/07-30-local" \
         .trellis/tasks/07-30-modular-harness-native-swarm-runtime-refresh
```

**Check:** summarize what differs and **present it** — do not auto-pick a
winner. The tracked (branch) version is the default keeper; only restore a local
file if the diff shows it carries work the branch version lacks, and say so
explicitly in the report.

## Step 7 — Full quality gate on merged `main`

Run in order; stop at the first failure.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace --jobs 1 --exclude hya-e2e
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1          # expect 19 passed / 0 failed
bash scripts/verify-no-http.sh
```

Then Track T:

```sh
cargo build --locked -p hya -p hya-backend -p hya-ts --bins
cd packages/hya-tui-ts && bun install --frozen-lockfile && bun run typecheck && bun run build && bun test
```

**Check:** every command exits 0 and Track P reports 19/19.

If something fails: report the actual output verbatim, do not patch production
code inside this task (PRD "Out of scope"), and do not proceed to step 8. A
pre-existing branch defect becomes its own task.

## Step 8 — REVIEW GATE: confirm before publishing

Report the full gate results and state plainly that the push publishes 75
commits / 517 files / +91,107 lines to `github.com/anyin233/hya`, and that
after it, rollback requires a revert or force-push. Wait for explicit
confirmation.

## Step 9 — Push

```sh
git push origin main
git rev-list --left-right --count main...origin/main    # expect: 0	0
```

## Step 10 — Record the worktree decision

Per `design.md`, the recommendation is to keep
`.worktrees/modular-harness-native-swarm-runtime-refresh` until children 2–5
finish (its warm `target/` saves a full rebuild), then remove it with
`git worktree remove`. Record whichever the user chooses in this task's notes —
an undecided worktree is not an acceptable end state.

## Step 11 — Close out

- Update `.trellis/tasks/08-05-e2e-suite-hardening/prd.md`: the baseline row
  "Suite location … does **not** exist on `main`" is now false; correct it.
- Note in this task whether the gate revealed anything the sibling children need
  to know (especially child 2, which encodes step 7's command sequence into CI).
- Run the Trellis quality check, then the commit step.

## Rollback points

Backups live at `/home/yanweiye/yaca-premerge-backup-2026-08-05` (durable), with
a working copy in `$SCRATCH/pre-merge-backup`. The scratchpad copy is under
`/tmp` and can be wiped at boot — always restore from the durable path.

| After step | How to undo |
| --- | --- |
| 3 | `git apply <backup>/main-dirty-binary.patch` — **the `--binary` patch, not `main-dirty.patch`**, which provably fails (`git apply` is atomic, so the plain patch restores *nothing*); then untar `untracked-all.tar.gz` and restore `07-30-local/` |
| 5, 6, 7 | `git reset --hard 156d0ad3`, then restore from backup as above. Note `reset --hard` does **not** touch untracked files, so the 86 untracked paths need the tar. |
| 9 | Revert commit or force-push — user decision, not this task's |

**Do not run `git clean -fd` at any point.** It is the natural reflex after an
interrupted checkout, and here it would delete 86 untracked files including this
task tree's own `.trellis/tasks/08-05-*` artifacts. Restore from the tar instead.
