# Design — retire unverified catalog test fixtures

Written after the survey (`research/catalog-fixture-survey.md`), which measured
the call sites rather than re-deriving the PRD's estimates. The survey changed
what this task should do: **three of the PRD's premises did not hold**, and the
largest requirement (R2) turns out to be zero work.

## Survey outcome

`BundleCatalog::from_prepared` has **exactly 99 call sites across 15 files**
outside `crates/hya-bundle/src/catalog.rs` — the PRD's figure was right.

Reachability to `prepare_spawn_admission`:

| Class | Count | Why |
| --- | ---: | --- |
| **CAN-REACH** | **1** | `crates/hya-app/tests/spawn_admission.rs:1639` |
| Wrong crate | 57 | `prepare_spawn_admission` is a private `fn` in `hya-app`; `hya-core`/`hya-bundle`/`hya-store`/`hya-server` do not depend on `hya-app`. Physically unreachable. |
| `hya-app/src/runtime.rs` unit tests | 37 | `engine_with_catalog` builds no spawn sender; last supervisor start precedes them all |
| `hya-store/src/bundle_registry.rs` | 2 | production validity checks, result discarded |
| `spawn_admission.rs:2503,2761` | 2 | spawn `resident: true`, so `all_transient` is false and they take the legacy route |

**The single CAN-REACH site is also the one the PRD says must not be migrated.**
`queued_spawn_uses_parent_turn_binding_after_catalog_publication` publishes an
unverified catalog *on purpose* and asserts the parent `TurnBinding` stays pinned
(assertions at `:1644-1650`, `:1657-1663`, `:1714-1721`). Migrating it would
destroy the test. It is also structurally impossible to migrate: it mutates
`PreparedBundle`s, while `from_verified_catalogs` takes `&[&PreparedCatalog]`.

**Net effect: R2 requires zero call-site migrations.** The PRD's fear — "any
future test routing a spawn through durable admission from one of them hits the
same wall" — is real as a *future* risk but has no present instance. Churning 99
sites was already ruled out by the PRD; the survey shows even the targeted
migration set is empty.

This makes R3 (dedupe the helper) the actual load-bearing work: it removes the
mechanism that let the original rot happen, which matters precisely because
there is no migration to do.

## What each requirement becomes

### R1 — survey. Done; published in `research/catalog-fixture-survey.md`.

### R2 — migrate reachable fixtures. **No-op, with the reason recorded.**
One reachable site, and it is deliberately unverified. Nothing to migrate.

### R3 — dedupe the helper. **The real fix.**
`verified_admission_test_runtime` (`spawn_admission.rs`) and
`support::test_runtime` (`tests/support/mod.rs`) have **byte-identical bodies**
(both md5 `e527d528bd4df179e2ff83e25050134d`); only the signature line differs.
`spawn_admission.rs` declares `mod support;` at line 3 **and never uses it** — the
module is dead today, which is exactly how the two copies drifted apart in
`994ea6b2`.

Delete `verified_admission_test_runtime`, rewire its 4 call sites (fanning out to
11 tests) to `support::test_runtime`.

### R4 — move `IdentityFakeProvider` into `tests/support/`.
At `nested_spawn_tree.rs:36-67`, used once. `async-trait` is already a normal
dependency, so no manifest change.

**Hard constraint, verified:** `FakeProvider` must not gain
`configured_identity_v1`. The assertion that a bare-`FakeProvider` router fails
closed is at **`crates/hya-app/src/runtime.rs:5021-5031`** — the PRD cites
`5016-5026`, which is line drift. `FakeProvider` has 290 references across 103
files (80 in `hya-server`); the move does not touch any of them.

### R5 — YAML id hazard. **The PRD's premise does not reproduce.**
The PRD asks to guard against YAML 1.1 bareword booleans (`no`, `on`, `y`).
A probe against `serde_norway 0.9.42` — the parser actually in use — found its
`parse_bool` accepts only `true/True/TRUE/false/False/FALSE`, and `deserialize_str`
bypasses tag resolution for `String` fields. `no`, `on`, `y`, `off`, `yes`,
`null`, `~` and `123` all round-trip **exactly**.

An assertion written against the bareword list would therefore guard nothing —
it would be security theatre that future readers trust.

The **real** silent-corruption inputs for this string-concatenated `bundle.yaml`
are different: `,` (splits `can_spawn`), a leading `&` (parsed as an anchor,
yielding an empty id), surrounding whitespace (trimmed), and `""`.

**Decision:** implement R5's intent — fail loudly on ids that would corrupt the
document — but against the hazards that actually exist, and record why the
bareword list was rejected. This is a deliberate deviation from the PRD's letter
in service of its purpose.

*Caveat carried from the survey: the probe tested `serde_norway` directly, not
`hya_bundle::prepare_builtins`, so hya-bundle's own validation may reject more
ids than the probe did — never fewer. The implementation must confirm the chosen
hazard set against the real prepare path, not against the probe.*

### R6 — bound the wait at `spawn_admission.rs:1681`.
It awaits a `JoinHandle` with two bare `expect`s, 9 lines after starting the
supervisor. A non-replying supervisor hangs the whole test binary. Local idiom is
`tokio::time::timeout(Duration::from_secs(5), …)` — see `:981-985` and
`:1169-1172`; 20 of the file's 25 timeouts use `from_secs(5)`.

## Scope corrections the PRD did not anticipate

- **Only one of the "two `harness_access` sites" needs the signature change**, and
  it needs a *second* change too — per-agent prompt overrides for the
  `ROOT_MAIN_BUNDLE_PROMPT` / `NESTED_CALLER_BUNDLE_PROMPT` markers. Changing the
  `(&str, AgentRole, &[&str])` tuple touches 12 call sites. Since R2 is a no-op,
  **this is deferred explicitly** rather than done — there is no reachable
  fixture that needs it.
- **The spec's own trigger is too narrow.** `.trellis/spec/backend/task-tool.md:218-221`
  says "background", but `uses_durable_admission_owner` returns `true` for
  foreground multi-member all-transient spawns too
  (`if req.background { len()==1 } else { true }`). Worth correcting while here,
  since this task's whole purpose is to stop the next person hitting this wall.

## Blast radius

Test-only, plus one spec doc:

| File | Change |
| --- | --- |
| `crates/hya-app/tests/spawn_admission.rs` | delete duplicate helper, rewire 4 call sites, bound one wait |
| `crates/hya-app/tests/support/mod.rs` | receives `IdentityFakeProvider`; id validation |
| `crates/hya-app/tests/nested_spawn_tree.rs` | `IdentityFakeProvider` moves out |
| `.trellis/spec/backend/task-tool.md` | correct the durable-admission trigger |

No product code. `FakeProvider` unchanged.
