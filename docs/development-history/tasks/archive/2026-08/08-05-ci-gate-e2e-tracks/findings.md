# Findings — CI gate wiring

## The cascade fix is verified, not assumed

`implement.md` step 6 required *observing* that one failing gate step no longer
skips the rest. Done on a throwaway branch `ci-cascade-probe`, which carried a
deliberate formatting-only violation (extra spaces in a `fn` signature —
semantically identical, so only `fmt` should object). Confirmed locally first:
`cargo fmt --check` reported 1 diff while the crate still compiled and tested
clean.

Run **31055282111** (`workflow_dispatch` on `ci-cascade-probe`):

| # | Step | Result | Under the old workflow |
| --- | --- | --- | --- |
| 8 | Test TypeScript TUI (Track T) | success | success |
| 9 | TUI smoke (non-gating) | success | — |
| 10 | **fmt** | **failure** (deliberate) | failure |
| 11 | clippy | success | **skipped** |
| 12 | build | success | **skipped** |
| 13 | clean build artifacts | success | **skipped** |
| 14 | test | failure | **skipped** |
| 15 | build agent e2e binaries | success | **skipped** |
| 16 | **agent process e2e (Track P)** | **success** | **skipped** |
| 17 | install strace | success | **skipped** |
| 18 | verify-no-http | success | **skipped** |

Eight steps that would previously have been skipped ran and reported. The run's
conclusion is correctly `failure`. The probe branch was deleted afterwards.

`workflow_dispatch` had to be added to the trigger list to make this possible at
all: the workflow only fired on `push: [main]` and `pull_request`, so the plan's
original "push a scratch branch and let CI run" step could never have worked.
That was a defect in the plan, found while executing it.

## Outcome — `main` is fully green under the new gate

Runs 31056356655 (`0acfc919`) and 31056395527 (`31a66f8f`): **all 18 steps
success**, including step 16 `agent process e2e (Track P)`. Track P is now an
enforced gate, not a documented aspiration.

### Cost delta (PRD constraint)

| | Wall-clock |
| --- | --- |
| Last run of the old workflow (`b64118bf`) | 935s |
| First green run of the new workflow (`31a66f8f`) | 978s |
| **Delta** | **+43s (+4.6%)** |

Per-step, for the four steps this task added:

| Step | Time |
| --- | --- |
| Test TypeScript TUI (Track T — 3 scenarios) | 1s |
| TUI smoke (non-gating, the rest of the bun suite) | 37s |
| build agent e2e binaries | 31s |
| agent process e2e (Track P) | 23s |

Narrowing Track T from a blanket `bun test` (37s) to the three registered
scenarios (1s) roughly pays for Track P's 54s. At +4.6% there is no case for
splitting into parallel jobs (`design.md` option A) — that stays a documented
alternative, not a pending action.

## What the new gate immediately caught

Two pre-existing flakes, both previously invisible because the steps that would
have exposed them were being skipped.

### 1. `hya-sdk` env-var race — FIXED (`0acfc919`)

`main` run 31055250033 failed at `test`:

```
crates/hya-sdk/src/server.rs:371  assertion `left == right` failed
  left:  Some("/home/runner/.local/state/hya/sessions.db")
  right: Some("/tmp/custom-hya-sessions.db")
```

Three tests in that module mutate the process-global `HYA_DB` env var, and cargo
runs a binary's tests on parallel threads. One test clears the variable while
another sits between its `set_var` and its assertion — the failure shows exactly
that, the explicit-env test reading the default state-dir path.

Reproduced locally by running only those three (maximizing overlap):
**1 failure in 15 runs**. After adding a module-level `ENV_GUARD` mutex:
**0 failures in 25 runs**, `hya-sdk --lib` 43/43, fmt and clippy clean.

The guard ignores mutex poisoning deliberately — a panic in one env test must
not cascade into spurious failures in the others.

### 2. `frontend_cli` ETXTBSY — recorded, NOT fixed

Probe run 31055282111, step 14:

```
crates/hya/tests/frontend_cli.rs  missing_adjacent_launcher_reports_its_path
Error: Os { code: 26, kind: ExecutableFileBusy, message: "Text file busy" }
```

ETXTBSY is the classic race of executing a binary that is still held open for
writing elsewhere. Seen **once**; root cause not established. Not fixed and not
guessed at — it needs its own investigation.

## A correction worth carrying forward

Local gate runs during child 1 were executed against a working tree containing
**unrelated uncommitted changes** (`crates/hya-sdk/src/{reducer,store,types}.rs`
and others belonging to other in-flight tasks). CI tests the committed state.
The two trees are not the same thing, and "1324 passed locally" was therefore
not a statement about what CI would see. That difference is exactly where the
`hya-sdk` failure hid.

Anyone reasoning from a local green run should either commit or stash unrelated
work first, or say plainly that the local result covers a different tree.

## Prior CI assumption that was wrong

Parent PRD decision **D2** recorded the "needs `workflow` scope" caveat as
resolved. It was not: pushing `.github/workflows/ci.yml` was rejected with
`refusing to allow an OAuth App to create or update workflow ... without
'workflow' scope`, and `gh auth status` showed only `gist, read:org, repo`.
Unblocked by the user running `gh auth refresh --hostname github.com -s workflow`.
The assumption should have been tested with a trivial workflow push during
planning instead of being written down as settled.
