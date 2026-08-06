# Implement — Track P scenarios for swarm tools

TDD throughout, per `AGENTS.md`: every scenario must be observed failing for the
right reason before it passes.

## Step 1 — Learn the resident fixture shape from existing tests

The harness has never spawned a resident agent. Do not invent the setup.

Read, in this order:
- `crates/hya-core/tests/resident.rs`
- `crates/hya-core/tests/resident_recovery.rs`
- `crates/hya-core/src/resident.rs` — especially `inbox_cursor` and where inbox
  messages are injected into the handle's context

**Check:** you can state, in one sentence each, (a) what makes a spawn resident
rather than transient at the API level, and (b) the exact point at which an
inbox message becomes visible to the recipient's next model request. If either
is still vague, keep reading — the oracles depend on both.

## Step 2 — Extend FakeLlm with per-agent routing (harness change, no scenarios yet)

In `crates/hya-e2e/src/fake_llm.rs`:

1. Add `routes: Vec<(String, VecDeque<ScriptStep>)>` to `Shared`.
2. Add `FakeLlm::route(&self, marker: impl Into<String>, steps: Vec<ScriptStep>)`.
3. In `chat_completions`, serialize the body once, take the first route whose
   marker is a substring **and** whose queue is non-empty; else fall back to
   `scripts` unchanged.
4. Add `FakeLlm::route_remaining(&self, marker: &str) -> Option<usize>` so a
   test can assert a route was actually consumed.

**Check — this is the regression gate for the whole task:**

```sh
cargo test -p hya-e2e -- --test-threads=1     # expect 19/19, unchanged
cargo clippy -p hya-e2e --all-targets -- -D warnings
```

With no routes registered the behavior must be byte-identical. If any of the 19
changes, stop: the fallback is wrong.

## Step 3 — Prove the routing works before building scenarios on it

Add one throwaway-scoped test that registers two routes and asserts each agent
consumed its own queue (`route_remaining` reaches 0 for both, and neither
consumed the other's). Without this, every later failure is ambiguous between
"scenario wrong" and "routing wrong".

**Check:** the test fails if the routing falls back to the shared queue —
verify by temporarily disabling the route lookup.

## Step 4 — Resident teammate fixture

Extend `E2eEnvBuilder` with the minimum needed to spawn a resident teammate and
wait until it is live. Reuse existing wait helpers; add a mailbox-specific wait
only if no existing one fits.

**Check:** `/session/{id}/tree` shows the teammate, and the teammate's own
FakeLlm route receives at least one request — proving it is actually running a
turn loop, not merely registered.

## Step 5 — Scenarios, one at a time, TDD

Order matters: `roster` first (cheapest oracle, proves the fixture), then
`send`, then channels.

| Order | ID | Tool | Oracle (per `design.md`) |
| --- | --- | --- | --- |
| 1 | T2.4 | `roster` / `list_agents` | caller's follow-up request contains handle + agent type |
| 2 | T2.5 | `send` direct | **recipient's** next request body contains the message marker |
| 3 | T2.6 | `send` `#channel` | recipient's request contains it; receipt `recipients` non-empty |
| 4 | T2.9 | `channels` | follow-up shows channel with member/message counts |
| 5 | T2.10 | `join` | after joining, a channel send reaches the joiner |
| 6 | T2.11 | `leave` | negative proof, with the positive control from `design.md` |

For each: write the assertion first, watch it fail for the *expected* reason
(not a setup error), then wire the scenario.

**Check per scenario:** `cargo test -p hya-e2e --test p16_swarm_mailbox -- --test-threads=1`

## Step 6 — The `leave` negative control

Implement exactly the five-step shape in `design.md`: a still-subscribed member
acts as the delivery clock, and only after *its* request shows the message may
the test assert the departed member never saw it.

**Check:** temporarily re-join the departed member and confirm the test fails.
A negative test that cannot fail is not a test.

## Step 7 — Register the scenarios

Add every new ID to `crates/hya-e2e/matrix.toml` and
`docs/testing/agent-matrix.md`. Update the built-in tool coverage statement
(8/25 → at least 14/25).

**Coordinate with child 5** (`08-05-matrix-toml-runner`) on the `T2.4`–`T2.6`
numbering before landing — it owns defining or retiring those gaps, and this
task is claiming three of them.

## Step 8 — Full verification

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1        # 19 existing + new, all green
cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast
```

**Check:** the 19 pre-existing scenarios still pass. That is the regression
floor stated in the PRD.

Note: run this against a tree without unrelated uncommitted changes, or say
explicitly that the result covers a different tree than CI will — that
distinction hid a real failure during child 1 (see
`.trellis/tasks/08-05-ci-gate-e2e-tracks/findings.md`).

## Step 9 — Land

Commit the harness change separately from the scenarios, so a later bisect can
tell "routing broke" from "a scenario broke". Push; CI now enforces Track P, so
the run itself is the final check.

## Rollback

Additive. Revert the scenario file, the harness routing, and the registry
entries; the pre-existing suite is untouched by design.
