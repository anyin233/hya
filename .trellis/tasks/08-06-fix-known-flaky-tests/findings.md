# Findings — the four known flaky tests

Written as results land. Every flakiness claim below states its run count, per
the PRD constraint that a single green run proves nothing.

## Correction to the PRD's scope: item 1 is a family of four, not one test

The PRD names one flaky `hya-app` test. Reproducing it exposed three more in the
same area. Under 24-way CPU saturation, 60 runs of `cargo test -p hya-app --lib`:

| Test | Failures / 60 runs |
| --- | ---: |
| `recovered_promotions_reconstruct_each_parent_binding` | 9 |
| `foreign_promotion_is_wake_only` | 4 |
| `recovered_transient_launch_executes_only_after_started_barrier` | 3 |
| `engine_build_fences_running_resident_and_resumes_queued_mail_before_readiness` | 1 |
| **Runs with ≥1 failure** | **15 / 60 (25 %)** |

**Three of the four** share one signature — an admission resolution returning
**0 where 1 is asserted** (`runtime.rs:6001`, `:6255`) — which points at a single
shared cause rather than independent bugs.

`foreign_promotion_is_wake_only` is **not** in that group: it fails as
`Elapsed(())` on a store-state poll ("A finalize must commit B's promotion"),
which is a different mechanism entirely and is treated separately below. Grouping
it with the other three would have pointed the investigation at the wrong place.

This is a **stronger reproduction condition than the PRD had**: the previous
round could only say "once in two full-workspace runs" and needed a 535 s
workspace run to see it. Under artificial load a 2 s lib-suite run reproduces it
at 25 %, which makes before/after measurement practical.

## Item 3 — `bundle_cli` temp-path collision — ROOT CAUSE ESTABLISHED

All 7 tests in `crates/hya-backend/tests/bundle_cli.rs` derive their data root
from the identical `hya-backend-bundle-cli-{pid}-{nanos}` string. `pid` is
constant within the binary, so `nanos` is the only discriminator — and it is not
unique across threads that start together:

| Probe (200 000 rounds, 7 threads released on a barrier, one `SystemTime::now().as_nanos()` sample each) | Result |
| --- | ---: |
| Rounds where ≥2 threads observed the **identical** value | **3 187 (1.59 %)** |
| Minimum observed delta between two threads | **0 ns** |

Cargo starts a binary's tests on parallel threads at essentially the same
instant — exactly the probe's shape. 1.59 %/run is consistent with the reported
"once in 24 whole-file runs" (≈4 %).

**Negative result, recorded honestly:** 40 sequential whole-file runs of
`bundle_cli` produced **0 failures**. At 1.59 %/run the expected count over 40
runs is ~0.6, so 0 is an unremarkable outcome and is *not* evidence against the
mechanism. The probe is the evidence; the suite run is not sensitive enough to
be one. Recorded so this number is not later misread as "could not reproduce".

## Item 2 — `frontend_cli` ETXTBSY — ROOT CAUSE ESTABLISHED

`crates/hya/tests/frontend_cli.rs:119-125` writes a copy of the `hya` binary and
immediately execs it. `fs::write` closes its own descriptor, so the writing
thread is clean; the race is cross-thread:

1. Thread A's `fs::write` holds a write descriptor on the new inode.
2. Another of the file's 6 parallel tests calls `Command::spawn`; the
   fork/`posix_spawn` child copies the descriptor table and transiently holds a
   **duplicate write descriptor** on A's inode — `O_CLOEXEC` clears it at
   `exec`, not at `fork`.
3. While that window is open the inode's `i_writecount` is non-zero, so A's
   `execve` returns `ETXTBSY`.

| Probe (3 000 write→exec attempts, 5 concurrent spawning threads) | Result |
| --- | ---: |
| `ETXTBSY` | **554 (18.5 %)** |

The mechanism reproduces on demand. This is a root cause, not a hypothesis.

Two facts that constrain the fix, both **verified rather than assumed**:

- `crates/hya/src/main.rs:5` derives the adjacent-launcher path from
  `std::env::current_exe()`. On Linux `/proc/self/exe` resolves to the path used
  at `execve`, so a **hard link** reports the link's own path — which is what the
  test asserts. A symlink would resolve back to the original binary and break the
  assertion.
- `/tmp` is on `/dev/nvme1n1p3` and `target/` on `/dev/sdf` — **different
  filesystems**, so `hard_link` into `std::env::temp_dir()` fails `EXDEV`. The
  isolated root must move to `CARGO_TARGET_TMPDIR`.

### Items 2 and 3 — fixes landed and proven by mutation

A green run proves nothing here: **both files were already green before the fix**
(40/40 and 7/7). The proof is that breaking the fix on purpose restores the
failure. Both gates were run against probes built outside the repo:

| Gate | Before (broken on purpose) | After (shipped form) |
| --- | ---: | ---: |
| `bundle_cli` path uniqueness — 7 threads, 200 000 rounds | **1 336 collisions (0.668 %)** | **0 (0.000 %)** |
| `frontend_cli` ETXTBSY — write→exec with 5 spawning threads | **90/3 000 (3.00 %)**, repeat **116/3 000 (3.87 %)** | **0/3 000**, and **0/10 000** |

The before-numbers are lower than my own probes (1.59 % and 18.5 %) because the
two sets ran at different machine loads. Same mechanism, different firing rate —
the after-numbers (0 in 200 000 and 0 in 13 000) are what carry the conclusion.

Verified independently of the implementer's report, by reading the diff:

- `hard_link` failures **propagate** (`?`); there is no copy fallback, which
  would have restored the race on exactly the machines where it bites.
- `set_permissions` was removed at that site — re-adding it would mutate the
  real cargo-built binary's inode, since the link shares it.
- `temp_dir` uses `create_dir`, not `create_dir_all`, so a collision still fails
  loudly rather than silently handing two tests one root.
- The `PermissionsExt` import is retained because `executable()` still uses it.

### Leftover, recorded rather than silently fixed

`bundle_cli`'s data roots are still never removed — the tests have no `Drop`
guard, so each run leaves a directory in `std::env::temp_dir()`. This is
**pre-existing and unchanged** by this task; the fix addressed uniqueness, not
cleanup. Recorded here so it is a known accepted cost rather than a surprise.

## Item 1 — root cause investigation

### Ruled out

- **Temp-path collision** (the mechanism behind items 2 and 3). The helper these
  tests use, `runtime.rs:10029 tempdir()`, *already* carries an atomic serial
  (`NEXT_TEMP_ID`) alongside pid and nanos. Two concurrent tests cannot share a
  workdir. Ruled out by reading the helper — the other items' explanation does
  **not** transfer to this one.
- **A shared database.** `engine_with_catalog` (`runtime.rs:10990`) builds every
  engine on `SessionStore::connect_memory()`, so each test owns a private
  in-memory DB. Cross-test journal interference is impossible.
- **The `16bde844` guard**, ruled out in the previous round on call-graph
  grounds.

### Leading hypothesis — process-global state mutated by sibling tests

`runtime.rs:9976-10026` defines an `EnvGuard` that, under a shared `ENV_LOCK`,
mutates **process-global** `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` *and* the
process **current directory**, restoring them on drop. Eleven tests take it.

`ENV_LOCK` serialises those eleven against each other — but **not** against the
other 143 tests in the binary, which run concurrently and observe the mutated
globals. None of the four flaky tests takes the lock.

A concrete path to the observed symptom: one `EnvGuard` test is named
`agent_with_model_omits_process_cwd_skill_index`, confirming a skill index that
depends on the process CWD. The flaky tests write skills into their workdir and
assert on the resulting runtime fingerprint; if CWD or `XDG_DATA_HOME` flips
mid-test, resolution yields a different fingerprint, `resolve_one` returns
`SpawnError::Unavailable`, and `resolve_admission_launches_fifo` drains to an
empty result — exactly `left: 0, right: 1`, with the error swallowed by
`.map_err(|_| SpawnError::Unavailable)`.

This is a hypothesis under test, **not** a diagnosis.

### Experiment, with predictions fixed in advance

Predictions were written down before the runs finished, so that a green result
cannot be retroactively read as confirmation:

| # | Setup (30 runs each, under 24-way load) | Prediction if hypothesis holds |
| --- | --- | --- |
| A | full lib suite, `--test-threads=1` | green — serialising removes the interference |
| B | only the 4 flaky tests, parallel | green — they do not interfere with each other |
| C | the 4 flaky tests **+ the 11 `EnvGuard` tests**, parallel | **red** — implicates `EnvGuard` |

If C comes back green, the hypothesis is wrong and the interferer is elsewhere
among the other 143 tests.

**Decision rule, also fixed in advance (PRD R4):** reproduced and understood →
fix the mechanism; not reproduced → record "not established", list what was
ruled out, and take the quarantine-vs-accept decision explicitly.

### Results — the hypothesis was FALSIFIED

| # | Setup (30 runs each, 24-way load) | Predicted | Observed |
| --- | --- | --- | --- |
| A | full suite, `--test-threads=1` | green | **0 `0 != 1` failures** ✓ (plus 4 unrelated timeouts, below) |
| B | the 4 flaky tests only, parallel | green | **0 / 30** ✓ |
| C | the 4 flaky + the 11 `EnvGuard` tests | **red** | **0 / 30** ✗ |

**C refutes the `EnvGuard` explanation.** Running every process-global-mutating
test concurrently with all four flaky tests, 30 times under load, produced no
failure at all. `EnvGuard` is not the interferer. The hypothesis is dropped, not
softened — writing the prediction down beforehand is what makes this a result
rather than a rationalisation.

### What A + B + C *do* establish

Comparing against the 12/60 baseline for the `0 != 1` family in the full parallel
suite:

- **It requires concurrency.** Serialised, 0/30. So it is not an intrinsic bug in
  any one test.
- **The four do not interfere with each other.** 0/30 with only them running.
- **The interferer is elsewhere** among the remaining ~139 tests — or is not a
  specific test at all, but the level of concurrent tokio-runtime contention that
  only the full 154-test suite produces.

That last distinction matters and is not yet settled: B and C ran 4 and 15 tests,
which never approach the contention of 154 tests across 32 threads.

### Next step — instrument rather than bisect

A bisect over ~139 tests is ~8 rounds × 30 runs. Instrumentation is decisive and
cheaper, because the failure is *silent by construction*:
`resolve_recovered_admission_launches` funnels seven distinct failure causes into
one `SpawnError::Unavailable` via `.map_err(|_| …)`, discarding the reason, and
`resolve_admission_launches_fifo` has a bare `continue` that drops a launch with
no signal at all. So the test can only ever report `0 != 1`.

Temporary `FLAKEPROBE` instrumentation was added at all seven sites plus the
`continue`, and the suite is being run under load until it fires. `runtime.rs` is
backed up (md5 `96b544fa…`) and the instrumentation is reverted before commit.

### ROOT CAUSE ESTABLISHED — and experiment C was a false negative

The instrumentation fired on run 11 and named the cause outright:

```
FLAKEPROBE ral: fingerprint mismatch
  runtime  intent=[170,247,113,…] recomputed=[110,18,41,…] same=false
  workdir="/tmp/hya-app-runtime-test-…2822901801-75-543933/recovered-transient-…"
  cwd=Ok("/tmp/hya-app-runtime-test-…3290920600-116-543933")
```

The process CWD (`tempdir()` serial **-116**) belongs to a **different test** than
the one resolving (serial **-75**). An `EnvGuard` was active in another thread.

**This reverses the earlier "falsified" verdict.** Experiment C's 0/30 was
**underpowered, not exculpatory**: it ran 15 tests, where the overlap between an
`EnvGuard`'s short mutation window and a flaky test's resolution window is rare.
The full suite runs 154 tests on 32 threads, where that overlap is common. A
green experiment retired the hypothesis prematurely; the direct capture overrides
it. Recorded because the mistake is instructive — a negative result from an
underpowered experiment is not evidence of absence.

### The mechanism, confirmed in code

1. `hya_tool::skill_dirs_for_workdir` (`crates/hya-tool/src/skill_catalog.rs:46-61`)
   builds the skill search path from the process-global **`HOME`**, appending
   `$HOME/.config/hya/skills`, `$HOME/.claude/skills`, and four more.
2. Those skills land in the runtime snapshot, which
   `TurnBinding::semantic_fingerprint_v1`
   (`crates/hya-core/src/runtime_registry.rs:553-578`) hashes.
3. Durable spawn admission records that fingerprint in the intent at claim time,
   then **recomputes it at resolution time** and compares
   (`runtime.rs:2169-2200`).
4. `EnvGuard::set` repoints `HOME` (and CWD) while 11 other tests run.

If step 4 lands between steps 3a and 3b, the two fingerprints disagree and
resolution fails closed as `SpawnError::Unavailable`, which the caller reports
only as a resolved-launch count of **0**.

This also explains the **machine dependence**: the mismatch requires the real
`HOME` to contain at least one skills directory that the temporary `HOME` lacks.
On a machine with no `~/.claude/skills`, both fingerprints would agree and the
flake would never appear — which is consistent with it being seen once in CI and
never reproduced by the previous round.

### The fix

`ENV_LOCK` becomes a `RwLock`:

- `EnvGuard::set` takes the **write** side (unchanged behaviour, one writer).
- A new `StableEnvGuard` takes the **read** side. It mutates nothing; it only
  pins `HOME` for tests whose assertions span a fingerprint capture and a
  fingerprint recomputation.

Readers still run concurrently with each other, so the cost is exclusion against
11 writers, not serialisation of the suite. Poisoning is ignored on both sides,
matching the `hya-sdk` `ENV_GUARD` precedent from `0acfc919` — a panic in one
environment test must not cascade.

Applied to the three tests that actually round-trip a fingerprint through
admission resolution, found by searching for callers of
`resolve_recovered_admission_launches` / `resolve_current_admission_launches`
rather than by patching the tests that happened to flake:

- `recovered_mismatch_aborts_once_and_resolves_promoted_match` (had not yet
  flaked, but is exposed by the identical mechanism)
- `recovered_promotions_reconstruct_each_parent_binding`
- `recovered_transient_launch_executes_only_after_started_barrier`

**The check pass found this set incomplete, and it was right.** Searching for
callers of the two *async* helpers missed the **synchronous**
`resolve_admission_launches` and `prepare_spawn_admission`, which recompute the
same fingerprint at `runtime.rs:2012` / `:2171`. Two further tests round-trip a
fingerprint through those and were equally exposed — unflagged only because they
had not happened to flake:

- `spawn_admission_prepares_canonical_intents_before_runtime_resolution`
  (asserts fingerprint equality directly at `:5205`)
- `accepted_admission_launches_resolve_without_touching_queued_members`

Guarded set is therefore **5 tests**, not 3. This is a good illustration of why
the set was derived by searching for the mechanism rather than by patching the
tests that flaked — the first search was simply not wide enough.

**Stated limitation, not an oversight.** Indirect exposure remains: the
supervisor's `promote` (`runtime.rs:2683`) calls
`resolve_current_admission_launches`, so any test driving a live spawn also
round-trips a fingerprint. Those are deliberately **not** guarded: std's
`RwLock` is writer-preferring, so putting dozens of long-lived readers behind the
11 `EnvGuard` writers would come close to serialising the whole 154-test suite —
a real, certain cost against a mechanism not observed on those paths. The
boundary is recorded here so a future failure on one of them is recognised
immediately rather than re-investigated from scratch.

### Before / after, same condition (24-way CPU saturation, `cargo test -p hya-app --lib`)

| | `0 != 1` family failures |
| --- | ---: |
| Before the fix | **12 / 60 runs (20 %)** |
| After the fix | **0 / 60 runs** |

Measured under the *same* reproducing condition, not a single green run.

### Two further flaky tests found, both OUTSIDE this PRD's four

Neither is in the PRD, and both surfaced only under the 24× CPU saturation I
induced. Recorded with an explicit decision rather than quietly fixed or quietly
ignored.

**1. `foreign_promotion_is_wake_only` — 6/60 after-fix runs, `Elapsed(())`.**
Not the `0 != 1` signature; a timeout. Its wait is a busy-spin
(`tokio::task::yield_now()` in a loop that hammers `store.admission()` as fast as
it can), which starves the very promotion task it is waiting for. Under 24×
oversubscription that is largely self-inflicted by the poll loop itself.

**2. `oauth::callback::tests::captures_code_and_state_from_callback` — 3/60,
`ConnectionRefused` (`callback.rs:160`).** This one is a **real race, provable by
inspection rather than by load**:

```rust
let listener = TcpListener::bind("127.0.0.1:0")?;   // take a port
let port = …; drop(listener);                        // release it — window opens
thread::spawn(move || wait_for_callback(…, port, …));// re-bind, eventually
thread::sleep(Duration::from_millis(50));            // *hope* it re-bound
TcpStream::connect(…)                                // ConnectionRefused
```

A fixed 50 ms sleep standing in for a readiness handshake, plus a
drop-then-rebind window in which any other process can take the port. This will
fire on a loaded CI runner regardless of my probe.

The correct fix is **not** a longer sleep and **not** a connect retry: it is to
remove the rebind window by having `wait_for_callback` accept an
already-bound `TcpListener`, so the test binds once and the server is listening
before `connect` is possible. That is an additive production signature change and
therefore outside this task's scope.

**Decision:** both are recorded here and neither is fixed in this task. They are
not among the PRD's four, and the second needs a production-side change that
deserves its own task rather than being smuggled into a test-only commit.

**Observation worth carrying forward regardless of outcome:** those
`.map_err(|_| SpawnError::Unavailable)` calls are a genuine diagnosability
defect. They are why this flake cost two investigations to get this far. Noted,
not fixed here — changing production error handling is outside this task's scope.

### Separately: a load artefact, not a CI flake

`foreign_promotion_is_wake_only` failed 4/30 in experiment A with
`Elapsed(())` at `runtime.rs:7948` — a **timeout**, not the `0 != 1` signature.
Its wait is a busy-spin (`tokio::task::yield_now()` in a poll loop), which
starves under 24× CPU oversubscription.

This appeared **only under artificial saturation I created**, and it is not in
the PRD's list of four. I am not claiming it as a CI flake on this evidence; an
unloaded baseline is needed before saying anything about it. Recorded so the
number is not mistaken for a real-world rate.

## Item 4 — `pty-smoke.test.ts`

No code change. Already non-gating as of `fee38938` (`continue-on-error: true`).
One failure (CI run 31053432077) on byte-identical code that passed on two other
runs; 3/3 locally. Per PRD R5 the status is recorded in
`docs/testing/agent-matrix.md` so a red step is understood rather than ignored.
Chasing a TypeScript PTY timing flake seen once is out of proportion now that it
cannot block the Rust gate.
