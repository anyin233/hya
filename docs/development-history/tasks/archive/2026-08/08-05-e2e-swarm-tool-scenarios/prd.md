# Add Track P scenarios for swarm tools

Child 3 of `08-05-e2e-suite-hardening`. Closes Gap 2. Depends on child 1;
lands cleanest after child 2 so new scenarios are gated on arrival.

## Goal

Give the native swarm tool surface — `send`, `roster`, `channels`, `join`,
`leave`, `list_agents` — process-level E2E coverage against a real backend, so
the feature line this release is named after cannot regress silently.

## Why this exists

`ToolRegistry::builtins()` registers 25 primary tool names. Track P exercises 8
(`read`, `write`, `edit`, `shell`, `question`, `skill`, `task`, `todowrite`).
The six swarm tools have in-process tests only
(`crates/hya-core/tests/resident.rs`, `crates/hya-store/tests/resident_claim.rs`,
`crates/hya-app/tests/spawn_admission.rs`,
`crates/hya-core/tests/role_selector_vs_can_spawn_roster.rs`), none of which
runs the production binary or the real HTTP/tool-dispatch path.

Multi-agent mailbox behavior is exactly the kind of thing in-process tests flatter:
delivery, membership, and roster visibility all depend on live session state
that a single-process fixture can fake into working.

## Requirements

- R1. Cover all six swarm tools with at least one Track P scenario each:
  - `roster` / `list_agents` — a spawned teammate is visible with the expected
    handle, agent type, session id, and status.
  - `send` — direct message to a teammate handle, and a message to a
    `#channel`; the receipt's `recipients` must reflect actual delivery.
  - `channels` — a created channel appears with correct member and message
    counts.
  - `join` / `leave` — membership changes the set of recipients a subsequent
    `send` reaches.
- R2. Oracles must prove delivery, not emission. Per
  `docs/testing/process-e2e.md` §"Oracle rules (do not weaken)": assert on the
  **recipient's** observable state (a follow-up FakeLlm request body containing
  the delivered message, or a server route reflecting membership), never on the
  sending tool's own call arguments or its self-reported success string.
- R3. `leave` must be verified negatively: after leaving, a channel `send` must
  demonstrably **not** reach the departed member. A test that only checks the
  "You no longer receive mail" string proves nothing.
- R4. Reuse `E2eEnvBuilder`. Extend the harness only where multi-agent setup
  genuinely requires it (e.g. deterministically spawning ≥2 live teammates and
  scripting FakeLlm per-agent); do not fork a parallel harness.
- R5. Register every new scenario ID in `matrix.toml` and
  `docs/testing/agent-matrix.md`, using the existing `T*.*` scheme. Coordinate
  numbering with child 5, which owns the `T1.1` / `T1.6` / `T2.4`–`T2.6` gaps.

## Constraints

- Scripting FakeLlm for two or more concurrently live agents is the hard part:
  the current `src/fake_llm.rs` is a single ordered queue of `ScriptStep`s
  shared by all requests. Multi-agent scenarios need either per-agent scripting
  or a routing key, and that harness change must not alter existing scenarios'
  behavior.
- Tests must stay deterministic under `--test-threads=1`. Mailbox delivery is
  asynchronous; use the existing wait helpers (`wait_session_idle`, and add a
  mailbox-specific wait if needed) rather than sleeps.
- Every existing Track P scenario must still pass unchanged — 19/19 is the
  regression floor.
- Swarm tools carry `ToolPermission::ReadOnly` (`channels`, `roster`,
  `list_agents`) versus write-ish paths; scenarios must not accidentally rely on
  `yolo(true)` to mask a permission regression.

## Acceptance criteria

- [ ] New `crates/hya-e2e/tests/pNN_*.rs` files cover all six swarm tools.
- [ ] Each scenario's oracle observes recipient-side or server-side state, and
      a reviewer can point at the specific assertion that would fail if delivery
      broke while the tool still returned success.
- [ ] The `leave` scenario fails if a departed member still receives channel
      mail — proven by temporarily inverting the behavior or the assertion.
- [ ] Every new test fails before the scenario is wired and passes after (TDD
      gate per `AGENTS.md`).
- [ ] `cargo test -p hya-e2e -- --test-threads=1` passes, with the pre-existing
      20 scenarios still green.
- [ ] `cargo clippy -p hya-e2e --all-targets -- -D warnings` clean.
- [ ] New IDs registered in `matrix.toml` and `agent-matrix.md`.
- [ ] Built-in tool coverage restated in `agent-matrix.md`: 8/25 → at least
      14/25.

## Out of scope

- The remaining uncovered tools (`ls`, `glob`, `find`, `grep`, `lsp`,
  `apply_patch`, `webfetch`, `websearch`, `plan_exit`, `ask_user`, `invalid`).
  Worth a follow-up task; not this one.
- Failure paths for swarm messaging (send to unknown handle, join a
  nonexistent channel, mailbox overflow). Note them as findings for the next
  round.
- Any behavioral change to the swarm tools themselves. If a scenario uncovers a
  real bug, file it and fix it under its own task rather than folding a
  production fix into a test task.

## Rollback

Additive. Revert the new test files plus the harness extension; the pre-existing
suite is untouched by design.
