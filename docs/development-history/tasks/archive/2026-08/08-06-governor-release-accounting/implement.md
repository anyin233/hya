# Implement — governor release accounting

**This is an investigation.** The deliverable is three recorded decisions with
evidence. Code changes are only those the decisions justify. A defensible
"still unknown, here is what would settle it" beats a speculative fix.

## Step 1 — Observation 2 first (cheapest, most concrete)

Do this one first: it is settled by reading plus one test edit, and it calibrates
the others.

- [ ] Verify the claim in `design.md`: `release_operation`
      (`crates/hya-core/src/orchestrator.rs:242`) removes the debit from
      `budgets.operations` **before** any arithmetic and returns `false` when
      absent — so a double release is a no-op and the boolean is a precise
      observable. **Confirm this in the code; do not take it from the design.**
- [ ] If confirmed: give
      `admitted_background_transient_releases_its_exact_debit_on_completion`
      (`crates/hya-app/tests/spawn_admission.rs:495`) an observable that actually
      proves exactness — first release `true`, second `false` — so the name
      becomes accurate.
- [ ] If a narrow test-only accessor is needed, add one. Do **not** change
      `release_operation`'s semantics or remove the `.min(per_run_budget)` clamp.
- [ ] If the claim does **not** hold, say so and fall back to R3's other branch:
      rename the test to what it actually checks.

**Prove it by mutation:** make the release path fire twice on purpose and confirm
the new assertion fails. A test that cannot fail proves nothing — that is exactly
the trap the previous round hit with the receipt oracle.

## Step 2 — Observation 1: is the window user-visible?

- [ ] Build the experiment R2 names: at `per_run_budget = 1`, admit a
      single-member batch, let the member finalize its journal row
      (`runtime.rs:1777`), and attempt a **second spawn on the same root** before
      the owner reaches `release_transient_operation` (`runtime.rs:3044`).
- [ ] Record the outcome with a run count, not a single observation.
- [ ] **Rejected `Overloaded`** → classify **real defect**. Record it; do NOT fix
      it here — the fix is a design question deserving its own task.
- [ ] **Admitted** → classify **accepted design** and record *what* closes the
      gap, so it is not re-discovered.
- [ ] If the window cannot be provoked deterministically, say so and state what
      evidence would settle it. That is an acceptable outcome.

**Hard constraint:** do **not** release the governor debit at member-finalize
time. The debit is `cardinality` units released as one unit by the owner;
per-member early release is wrong for multi-member batches. This is the
obvious-looking wrong answer and the PRD names it explicitly.

## Step 3 — Observation 3: record the invariant

- [ ] Confirm the ownership argument still holds: `SessionEngine` owns the
      `BoundSpawnSender` and the supervisor task holds an `engine.clone()`, so
      `rx.recv()` cannot return `None` while the supervisor lives, making the
      drain branch reachable only from the test helper `spawn_team_supervisor`.
- [ ] Record the invariant **where it will be seen when ownership changes** — a
      comment at the ownership site in `crates/hya-app/src/runtime.rs`, plus
      `.trellis/spec/backend/task-tool.md`. A task archive does not count; the
      PRD says so.
- [ ] State plainly that severity is contingent on that ownership: if it ever
      changes, the drain loop must become stop-aware, and
      `fail_after_claim`'s `std::future::pending::<()>()` (`runtime.rs:2842`) is
      why it matters.
- [ ] **Do not** make the whole supervisor stop-aware. Dead branch; scope creep
      buys nothing.

## Step 4 — write the decisions down

- [ ] `findings.md`: for each of the three, the decision (**real defect** /
      **accepted design** / **still unknown**), the evidence, and run counts for
      anything timing-dependent.
- [ ] Anything "still unknown" must state what evidence would settle it.
- [ ] Anything "accepted design" goes in `.trellis/spec/backend/task-tool.md`,
      where a future reader will find it.

## Step 5 — verification gate

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- matrix-check
cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
bash scripts/verify-no-http.sh
```

Redirect each to a file and grep it; **never pipe through `tail`**. Baseline:
**238 binaries, 1310 passed, 0 failed, 3 ignored**; e2e **18 binaries, 30
passed**. Anything materially smaller is an incomplete capture, not a shrunken
suite.

Spawn admission is easy to break silently — the full suite *and* Track P both
matter here, per the PRD.

## Step 6 — commit

Stage only the files in `design.md` § Blast radius. Never stage
`crates/hya-sdk/src/{reducer,store,types}.rs`,
`.trellis/tasks/07-23-remove-rust-tui/**`, or the `fixtures/*` / `imgs/*`
deletions — they belong to other in-flight tasks.

One-line semantic messages, atomic scope, no agent attribution. Version is now
`0.34.14`; bump again only if this task lands a user-visible product change,
which on the current plan it should not.
