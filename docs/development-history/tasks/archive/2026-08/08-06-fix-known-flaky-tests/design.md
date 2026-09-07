# Design — fix the four known flaky tests

This design is written *after* diagnosis, not before it. Each item below records
what was measured, and the fix follows from the measurement. Where a root cause
could not be established, that is stated instead of a guess (PRD R1).

## Diagnostic method

Two of the four were settled by a standalone probe that reproduces the suspected
mechanism in isolation, at a rate high enough to measure. This is deliberate:
the flakes themselves fire at ~1–4%, so a probe that isolates the mechanism
gives a far stronger signal than re-running the test suite and hoping.

Probes live in the session scratchpad (`nanos_probe.rs`, `etxtbsy_probe.rs`);
their results are transcribed here because they are the evidence.

---

## Item 3 — `bundle_cli` temp-path collisions — ROOT CAUSE ESTABLISHED

### Measurement

All 7 tests in `crates/hya-backend/tests/bundle_cli.rs` derive their data root
from the **identical** format string:

```rust
let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
std::env::temp_dir().join(format!("hya-backend-bundle-cli-{}-{nanos}", std::process::id()))
```

`std::process::id()` is constant across the binary, so `nanos` is the *only*
discriminator. The assumption that a nanosecond timestamp is unique across
threads is false on this machine:

| Probe | Result |
| --- | --- |
| 7 threads, released on a barrier, each sampling `SystemTime::now().as_nanos()` once; 200 000 rounds | **3 187 rounds (1.59 %) had at least two threads observe the identical value**; minimum observed delta **0 ns** |

Cargo starts a binary's tests on parallel threads at essentially the same
instant, which is exactly the probe's shape. A 1.59 % per-run collision rate is
consistent with the reported "once in 24 whole-file runs" (≈4 %).

### Why the reproduction attempt did not fire

40 sequential whole-file runs of `bundle_cli` produced **0 failures**. At
1.59 %/run the expected count over 40 runs is ~0.6, so 0 is an ordinary outcome
and is *not* evidence against the mechanism. The probe is the evidence; the
suite run is not sensitive enough to serve as one. This is recorded so the
number is not later misread as "could not reproduce, therefore not real".

### Fix

Add an intra-process atomic serial to the path, which makes uniqueness a
guarantee rather than a probability. This is the pattern **already used in this
codebase** at `crates/hya-app/src/runtime.rs:10029` (`tempdir()`), so it is a
convergence on an existing local idiom, not a new invention:

```rust
static NEXT_DATA_ROOT: AtomicU64 = AtomicU64::new(0);
let serial = NEXT_DATA_ROOT.fetch_add(1, Ordering::Relaxed);
```

The 7 duplicated inline blocks collapse into one `unique_data_root()` helper, so
the next test added to the file cannot reintroduce the collision by copy-paste —
which is how all 7 came to share the format string in the first place.

Explicitly **not** done: retrying `create_dir` on `AlreadyExists`. That would
convert a path-uniqueness bug into a silent path-reuse bug, where two tests
share a data root and corrupt each other's registry (PRD R2).

---

## Item 2 — `frontend_cli` ETXTBSY — ROOT CAUSE ESTABLISHED

### Mechanism

`crates/hya/tests/frontend_cli.rs:119-125`:

```rust
std::fs::write(&relocated, std::fs::read(env!("CARGO_BIN_EXE_hya"))?)?;
std::fs::set_permissions(&relocated, …0o755)?;
let output = Command::new(&relocated).output()?;      // ← ETXTBSY here
```

`fs::write` closes its descriptor before returning, so the *writing* thread is
clean. The race is cross-thread:

1. Thread A calls `fs::write(&relocated, …)`, holding a write descriptor on the
   new inode for the duration of the call.
2. Concurrently, one of the other 5 tests in the file calls `Command::spawn`.
   The fork/`posix_spawn` child copies the parent's descriptor table, so it
   transiently holds a **duplicate write descriptor** on A's inode.
   `O_CLOEXEC` clears it at `exec`, not at `fork` — so a window exists.
3. While that window is open the kernel's `i_writecount` for the inode is
   non-zero, and A's `execve` of `relocated` returns `ETXTBSY`.

### Measurement

A probe replicating exactly this shape — 5 threads spawning children in a loop,
1 thread writing an executable and immediately exec'ing it:

| Probe | Result |
| --- | --- |
| 3 000 write→exec attempts with 5 concurrent spawning threads | **554 ETXTBSY (18.5 %)** |

The mechanism reproduces on demand. This is a root cause, not a hypothesis.

### Fix

Stop exec'ing an inode that this process has opened for writing. Replace the
copy with `fs::hard_link` from `CARGO_BIN_EXE_hya`: the exec target is then the
cargo-built binary's own inode, which nothing writes during the test run, so
`i_writecount` is never non-zero and the window cannot exist.

Two facts make this viable, both verified rather than assumed:

- `crates/hya/src/main.rs:5` derives the adjacent-launcher path from
  `std::env::current_exe()`. On Linux `/proc/self/exe` resolves to the path used
  at `execve`, so a hard link reports the **link's** path — which is what the
  test asserts. A symlink would resolve to the original binary and break the
  assertion; a hard link does not.
- `/tmp` is on `/dev/nvme1n1p3` and `target/` on `/dev/sdf` — **different
  filesystems**, so `hard_link` into `std::env::temp_dir()` would fail `EXDEV`.
  The isolated root therefore moves to `CARGO_TARGET_TMPDIR`, which cargo places
  under `target/`, on the same filesystem as the binary.

`temp_dir()` also gains the same atomic serial as item 3. The six current
prefixes are distinct, so there is no collision today — this is hardening
against the next test, not a fix for an observed failure, and is labelled as
such.

Explicitly **not** done: retrying on `ETXTBSY`, or sleeping before the exec.
Both leave the race in place (PRD R2).

---

## Item 1 — `recovered_promotions_reconstruct_each_parent_binding` — OPEN

Observed once in two full-workspace runs; `resolve_recovered_admission_launches`
returned 0 resolved launches where 1 was asserted (`runtime.rs:6001`).

### Ruled out so far

- **Temp-path collision (the item-2/3 mechanism).** The helper this test uses,
  `runtime.rs:10029 tempdir()`, *already* carries an atomic serial
  (`NEXT_TEMP_ID`) alongside pid and nanos, so two concurrent tests cannot share
  a workdir. The mechanism that explains items 2 and 3 does **not** explain this
  one. Ruled out by reading the helper, not by inference from the other items.
- **The `16bde844` guard**, ruled out in the previous round: the assertion sits
  in `resolve_recovered_admission_launches`, which has no call relationship to
  `spawn_team_supervisor_with_environment`.

### Leading candidate, not yet established

`resolve_admission_launches_fifo` (`runtime.rs:2260`) silently `continue`s when
the stored record does not match:

```rust
if !records.iter().any(|record| {
    record == &launch.record && record.state == AdmissionState::Accepted
}) { continue; }
```

This is **full struct equality** on `AdmissionRecord`, which includes
`created_at` and `updated_at` (`crates/hya-store/src/admission.rs:111-112`).
Any concurrent touch of the row, or any timestamp the store recomputes on read,
makes the comparison fail and the loop drain to an empty result — producing
exactly the observed `left: 0, right: 1` with no error surfaced.

A second path to the same symptom: `resolve_one` fails for the owner launch,
`finalize_admission_members` promotes nothing, and the loop exits empty.

Both are consistent with the symptom. Neither is confirmed. **A reproduction is
running** (60 iterations of the `hya-app` lib suite under 24-way CPU
saturation); the outcome decides whether this item ships a fix or a recorded
"not established" per PRD R4.

### Decision rule, fixed in advance

- Reproduced and understood → fix the mechanism.
- Not reproduced in the loaded loop → record "not established", list what was
  ruled out, and take the R4 decision (quarantine vs accept) explicitly.

Committing to this rule *before* seeing the result is deliberate: it stops a
green loop from being retroactively read as "fixed".

---

## Item 4 — `pty-smoke.test.ts` — DOCUMENTATION DECISION

Already non-gating as of `fee38938` (`continue-on-error: true`). One failure on
CI run 31053432077 against byte-identical code that passed on two other runs;
3/3 locally.

No code change. The PRD (R5) asks only that the non-gating status be recorded in
`docs/testing/agent-matrix.md` so a red step is understood rather than ignored.
Chasing a TypeScript PTY timing flake that has fired once is out of proportion to
its cost now that it cannot block the Rust gate.

---

## Blast radius

All changes are confined to test files plus one documentation file:

| File | Change |
| --- | --- |
| `crates/hya-backend/tests/bundle_cli.rs` | 7 inline blocks → one unique helper |
| `crates/hya/tests/frontend_cli.rs` | hard link instead of copy; `CARGO_TARGET_TMPDIR`; serial |
| `docs/testing/agent-matrix.md` | record pty-smoke non-gating status |
| `crates/hya-app/src/runtime.rs` | only if item 1 is diagnosed |

No product code is touched unless item 1 turns out to be a product defect.
