# Research: `BundleCatalog::from_prepared` fixture survey (PRD R1–R6)

- **Query**: Classify all `BundleCatalog::from_prepared` call sites by whether the
  fixture can reach `prepare_spawn_admission`; gather evidence for R3–R6.
- **Scope**: internal (repo `/chivier-disk/yanweiye/Projects/yaca`, branch `main`)
- **Date**: 2026-08-06
- **Task**: pre-work for `.trellis/tasks/08-06-unverified-catalog-fixture-debt`

Everything below marked **[verified]** was measured by running the command shown
or by reading the cited lines. Anything marked **[inferred]** is reasoning that
was not executed.

---

## 0. Headline

| Question | Answer |
|---|---|
| Call sites outside `crates/hya-bundle/src/catalog.rs` | **99, across 15 files** — PRD figure confirmed **[verified]** |
| Sites that **CAN** reach `prepare_spawn_admission` | **1** (`crates/hya-app/tests/spawn_admission.rs:1639`), and it is a **DO-NOT-MIGRATE** site |
| Sites that **CANNOT** reach it | **98** |
| Net migration work implied by R2 | **zero call-site migrations.** The reachable set is empty once intentional sites are excluded. |

The debt is therefore *not* "migrate N fixtures". It is R3/R4/R5/R6 (dedupe, move
the fake, YAML guard, bounded wait) plus a guard so the next fixture that goes
durable does not silently rot again.

---

## 1. Exact site inventory **[verified]**

Two different greps were run because `grep from_prepared` over-counts: two lines
in `crates/hya-bundle/tests/catalog.rs` are *function names* containing the
string (`catalog_rejects_bundle_mcp_even_from_prepared_data` at :212,
`catalog_rejects_unsupported_hook_local_id_from_prepared_data` at :246), not
calls.

```
$ grep -rn "from_prepared" --include="*.rs" . | wc -l
104                       # includes 3 lines in catalog.rs + 2 fn names

$ grep -rn "from_prepared(" --include="*.rs" . | wc -l
102                       # call expressions only

$ grep -rn "from_prepared(" --include="*.rs" . \
    | grep -v "^crates/hya-bundle/src/catalog.rs" | wc -l
99                        # <-- the number in the PRD
```

Output was redirected to a scratch file and counted there; no `tail`/`head` was
used in the counting pipeline.

| File | Sites | Kind |
|---|---:|---|
| `crates/hya-app/src/runtime.rs` | 37 | in-crate `#[cfg(test)] mod tests` (starts line 4326) |
| `crates/hya-core/src/runtime_registry.rs` | 32 | in-crate unit tests |
| `crates/hya-bundle/tests/catalog.rs` | 9 | catalog contract tests |
| `crates/hya-core/tests/subagent.rs` | 4 | integration tests |
| `crates/hya-core/tests/agent_resource_view.rs` | 3 | integration tests |
| `crates/hya-app/tests/spawn_admission.rs` | 3 | integration tests |
| `crates/hya-store/src/bundle_registry.rs` | 2 | **production code**, not a fixture |
| `crates/hya-core/tests/support/mod.rs` | 2 | shared test helper |
| `crates/hya-server/tests/support/mod.rs` | 1 | shared test helper |
| `crates/hya-server/src/compat/reference_tests.rs` | 1 | in-crate unit test |
| `crates/hya-core/tests/root_turn_bundle_precedence.rs` | 1 | integration test |
| `crates/hya-core/tests/historical_agent_identity.rs` | 1 | integration test |
| `crates/hya-core/tests/fixed_system_agents.rs` | 1 | integration test |
| `crates/hya-core/src/test_support.rs` | 1 | shared test helper |
| `crates/hya-bundle/tests/validation.rs` | 1 | integration test |
| **Total** | **99** | across **15** files |

The 3 excluded sites in `crates/hya-bundle/src/catalog.rs` are the definition
(`:43`) and its two internal re-uses by `from_verified_catalogs` (`:139`) and
`with_verified_catalogs` (`:165`).

---

## 2. What "can reach `prepare_spawn_admission`" actually requires

Four independent gates, all verified against source:

1. **Crate visibility.** `prepare_spawn_admission` is a *private* `fn` in
   `crates/hya-app/src/runtime.rs:1998`. Only `hya-app` code can name it.
   ```
   $ grep -rn "prepare_spawn_admission" --include="*.rs" .
   crates/hya-app/src/runtime.rs:1998:fn prepare_spawn_admission(
   crates/hya-app/src/runtime.rs:3115:        let prepared = match prepare_spawn_admission(
   crates/hya-app/src/runtime.rs:5131:        let prepared = prepare_spawn_admission(
   crates/hya-app/src/runtime.rs:5264:        } = prepare_spawn_admission(
   ```
   Exactly one production caller (`:3115`) and two in-crate unit tests
   (`:5131`, `:5264`). **[verified]**

2. **Crate dependency direction.** Per `Cargo.toml` inspection, `hya-core`,
   `hya-bundle`, `hya-store` and `hya-server` do **not** depend on `hya-app`
   (dependency flows the other way: `hya-app` → all four). No test in those
   crates can reach the function, by construction. **[verified]**

3. **A running team supervisor.** The only production caller lives inside
   `ForegroundTransientAdmissionPreparation::run` (`runtime.rs:3065-3115`),
   which is only constructed at `runtime.rs:3456`, inside
   `spawn_team_supervisor_with_environment` (`runtime.rs:3348`). A fixture that
   never starts `spawn_team_supervisor` / `build_session_engine` cannot reach it.
   **[verified]**

4. **`uses_durable_admission_owner` must return `true`** (`runtime.rs:1962-1986`):
   ```rust
   if req.members.is_empty() { return false; }
   let all_transient = req.members.iter().all(|member| {
       let Ok(definition) = authorize_spawn_target(binding, &req.agents, caller, member) else { return false; };
       definition.spawn_lifecycle != SpawnLifecycle::Resident
           && !member.resident
           && !member.inline_agent.as_ref().and_then(|i| i.resident).unwrap_or(false)
   });
   if !all_transient { return false; }
   if req.background { req.members.len() == 1 } else { true }
   ```
   > **Spec correction.** `.trellis/spec/backend/task-tool.md:218-221` says the
   > trigger is "single member, non-resident, transient, **background**". The
   > code shows **foreground multi-member all-transient batches also go durable**
   > (`else { true }`). The `len() == 1` clamp applies only when
   > `req.background`. Worth fixing in the spec while this task is open.

Only when **all four** hold does the unverified catalog matter — and then only
if the *pinned `TurnBinding`* carries the unverified catalog, since
`prepare_spawn_admission` reads the fingerprint off the binding, not off
whatever catalog is currently published:
`runtime.rs:2012` → `engine.runtime_semantic_fingerprint_v1(binding)` →
`hya-core/src/engine.rs:424` → `binding.semantic_fingerprint_v1(&self.permission)`
→ `hya-core/src/runtime_registry.rs:554` `self.snapshot.catalog.semantic_identity_v1()?`
→ `hya-bundle/src/catalog.rs:52` `semantic_identity_v1: None`. **[verified]**

---

## 3. Per-site classification

### 3.1 CAN-REACH (1 site)

| Site | Test | Call path |
|---|---|---|
| `crates/hya-app/tests/spawn_admission.rs:1639` | `queued_spawn_uses_parent_turn_binding_after_catalog_publication` (`:1554`) | `scoped.spawn(...)` (`:1615`, foreground, 1 transient non-resident member) → `BoundSpawnSender` → forwarded at `:1665-1669` → `spawn_team_supervisor(...)` (`:1672`) → `runtime.rs:3437 uses_durable_admission_owner == true` → `runtime.rs:3456 ForegroundTransientAdmissionPreparation` → `run()` → `runtime.rs:3115 prepare_spawn_admission` |

**But the unverified catalog built at `:1639` is not the one admission reads.**
The queued request was bound to `old_binding` (`:1605`), captured from a runtime
built by `verified_admission_test_runtime` (`:1567`), i.e. via
`BundleCatalog::from_verified_catalogs`. The catalog built by
`from_prepared(&published_bundles)` at `:1639` is published at `:1640-1642`
*after* the request is queued, and the test's whole point is that the pinned
binding ignores it. So this site is CAN-REACH-in-file but the admission path
consumes a verified fingerprint. **[verified by reading the test end-to-end]**

### 3.2 CANNOT-REACH (98 sites), grouped by shared reason

| Group | Sites | Shared reason |
|---|---:|---|
| **A. Wrong crate — no `hya-app` dependency** | 57 | `hya-core` (32 in `src/runtime_registry.rs`, 4 `tests/subagent.rs`, 3 `tests/agent_resource_view.rs`, 2 `tests/support/mod.rs`, 1 each in `tests/root_turn_bundle_precedence.rs`, `tests/historical_agent_identity.rs`, `tests/fixed_system_agents.rs`, `src/test_support.rs`), `hya-bundle` (9 `tests/catalog.rs`, 1 `tests/validation.rs`), `hya-server` (1 `tests/support/mod.rs`, 1 `src/compat/reference_tests.rs`). `prepare_spawn_admission` is private to `hya-app`, and none of these crates depend on `hya-app`. Physically unreachable. |
| **B. Production code, not a fixture** | 2 | `crates/hya-store/src/bundle_registry.rs:194` and `:288`. Both are `BundleCatalog::from_prepared(&complete)?;` with the result **discarded** — a pure validity check before an install/uninstall commit. No catalog escapes into a runtime. Out of scope. |
| **C. `hya-app` in-crate unit tests that never start a supervisor** | 37 | All 37 sites in `crates/hya-app/src/runtime.rs` sit at lines 11043–14807, i.e. inside `#[cfg(test)] mod tests` (opens at `:4326`). The last `spawn_team_supervisor_with_environment` call in that module is at `:9704` and the last `build_session_engine` at `:10832` — **all before 11005**. Their shared engine helper `engine_with_catalog` (`:10990-11003`) builds a `SessionEngine` directly with no spawn sender and no supervisor. These tests exercise `resolve_spawn_member`, `resolve_recovered_resident_agent`, `AdmissionResolutionContext::capture`, and `BundleSidecar*` directly. The one `run_team` call in the range (`:12435`) has no spawn supervisor attached, and the one `SpawnerPlane::new()` (`:14657`) immediately drops its receiver. |
| **D. `hya-app` integration sites whose spawn is resident → legacy route** | 2 | `crates/hya-app/tests/spawn_admission.rs:2503` and `:2761`. Both fixtures *do* start a real supervisor (`:2553`, `:2790`), but both tests spawn with `resident: true` (`:2612`, `:2841`). `uses_durable_admission_owner` then fails `all_transient` and returns `false` (`runtime.rs:1970-1980`), so the request takes the legacy route and never reaches `prepare_spawn_admission`. |

Group-C helper-fn cross-check **[verified]**: three of the 37 sites are inside
helpers rather than tests — `catalog_with_agents` (`:11005`),
`catalog_with_worker_policy` (`:11151`), `cross_bundle_selector_catalog`
(`:13972`). Their callers were enumerated
(`grep -rn "catalog_with_agents\|catalog_with_worker_policy\|cross_bundle_selector_catalog"`)
and every one — `runtime.rs:4625, 11049, 11086, 11132, 11330, 14237, 14414` —
resolves to a test that calls the target function directly, with no supervisor.
Note `:4625` is the only caller *outside* the 11000+ block; its test
(`admission_binding_base_fields_match_reconstructed_agent`, `:4623`) calls
`AdmissionResolutionContext::capture` and `agent_spec_for_binding` directly and
passes a hard-coded `runtime_fingerprint = [0x5a; 32]` (`:4629`), so it never
consults the catalog identity at all.

**Adjacent risk worth knowing [verified]:** three `hya-core` unit tests *do*
require a verified catalog and would panic loudly (not fail closed silently) if
someone downgraded them — `runtime_registry.rs:2382`, `:2564`, `:2761`, each
with `panic!("... must be fingerprintable")` at `:2482/:2664/:2878`. All three
already use `from_verified_catalogs` (`:2416`, `:2627`, `:2816`). They are
correct today; listing them so nobody "simplifies" them into Group A.

---

## 4. Explicit exclusions — DO-NOT-MIGRATE

### 4.1 `crates/hya-app/tests/spawn_admission.rs:1639` — deliberate unverified publication

```rust
1636    assert_eq!(quick.prompt.as_deref(), Some(OLD_QUICK_PROMPT));
1637    quick.prompt = Some(NEW_CATALOG_CHILD_PROMPT.to_string());
1638    let published_catalog =
1639        BundleCatalog::from_prepared(&published_bundles).expect("complete replacement catalog");
1640    runtime
1641        .publish_catalog(Arc::new(published_catalog))
1642        .expect("publish replacement catalog");
```

Assertions that prove the intent:

```rust
1644    assert_eq!(
1645        old_binding.resolve_agent("quick").and_then(|agent| agent.prompt.as_deref()),
1648        Some(OLD_QUICK_PROMPT),
1649        "the parent TurnBinding must remain pinned"
1650    );
...
1653    assert_ne!(fresh_generation, old_generation,
1655        "catalog publication must advance the runtime generation");
...
1657    assert_eq!(
1658        fresh_binding.resolve_agent("quick").and_then(|agent| agent.prompt.as_deref()),
1661        Some(NEW_CATALOG_CHILD_PROMPT),
1662        "a fresh TurnBinding must observe the published catalog");
...
1714    assert!(child_system.contains(OLD_QUICK_PROMPT),
1716        "queued child must use the parent binding's OLD prompt: {child_system}");
1718    assert!(!child_system.contains(NEW_CATALOG_CHILD_PROMPT),
1720        "queued child must not rebind to the NEW catalog prompt: {child_system}");
```

Why migration is not merely undesirable but **structurally impossible with the
current API [verified]**: the replacement catalog is built from *mutated*
`PreparedBundle` values (`:1630-1637` clones the old binding's bundles and
rewrites `quick.prompt`). `from_verified_catalogs` takes `&[&PreparedCatalog]`
(`catalog.rs:134`) and `with_verified_catalogs` requires pre-existing verified
records (`catalog.rs:150-154`). There is no supported way to produce a *verified*
catalog carrying a hand-mutated agent prompt. Leave it.

Also flagged by the PRD as mitigation to preserve: `:1636`
`assert_eq!(quick.prompt.as_deref(), Some(OLD_QUICK_PROMPT))` — this is the
guard that makes helper drift fail loudly. **[verified, still present]**

### 4.2 `crates/hya-app/tests/spawn_admission.rs:2503` — per-agent `harness_access` divergence

`nested_root_divergence_runtime` (`:2423-2508`) exists specifically to make the
*root* agent `HarnessAccess::Full` and the *nested caller* `HarnessAccess::None`
(`:2465`, `:2473`), and asserts on that divergence at `:2692-2697`. Not
expressible by the shared helper (see §5). Also unreachable today (resident
spawn). Leave it, or migrate only alongside a helper-signature change.

### 4.3 `crates/hya-app/tests/spawn_admission.rs:2761` — no divergence, but no benefit

`missing_root_definition_fails_before_admission_for_resident_batch` (`:2703`)
builds a catalog whose agents are all `HarnessAccess::Full` (`:2734`) and whose
root agent id is deliberately absent so root activation fails
(`:2718-2719` comment; `:2851` asserts `SpawnError::UnknownAgentId { "ghost-root" }`).
It is unreachable (resident batch) and the fail-closed assertion it makes is
about `UnknownAgentId`, not about the fingerprint. No reason to touch it.

---

## 5. The two `harness_access` sites (PRD constraint) — **partial correction**

The current shared helper signature is:

```rust
// crates/hya-app/tests/support/mod.rs:9-12
pub fn test_runtime(
    tools: Arc<ToolRegistry>,
    agents: &[(&str, AgentRole, &[&str])],
) -> Arc<RuntimeRegistry> {
```

and it hard-codes both the access level and the prompt:

```rust
// crates/hya-app/tests/support/mod.rs:22-24
manifest.push_str(&format!(
    "  - local_id: {stable_id}\n    stable_id: {stable_id}\n    role: {role}\n    prompt: prompts/{stable_id}.md\n    spawn_lifecycle: transient\n    harness_access: full\n"
));
// :30-33 — prompt body is always `format!("{stable_id} prompt")`
```

The two sites the PRD refers to are `spawn_admission.rs:2503` and `:2761` — the
only two remaining hand-built `PreparedAgent` fixtures in `hya-app/tests`. But
they are **not symmetric**, and the PRD's framing is slightly off:

| Site | Sets `harness_access` | Actually *diverges* | Migratable with today's signature? |
|---|---|---|---|
| `:2503` (`nested_root_divergence_runtime`) | per-agent: `Full`, **`None`**, `Full`, `Full`, `Full` (`:2465,:2473,:2480,:2487,:2494`) | **yes** | **No.** Needs (a) a `HarnessAccess` field in the tuple, **and** (b) custom prompt bodies — the test asserts on `ROOT_MAIN_BUNDLE_PROMPT` / `NESTED_CALLER_BUNDLE_PROMPT` marker constants (`:2417-2418`, asserted at `:2662`, `:2666`), which the helper cannot produce. |
| `:2761` (inline closure) | uniformly `HarnessAccess::Full` (`:2734`) | no | **Yes, mechanically** — the helper already emits `harness_access: full` and prompt body `"{stable_id} prompt"`, matching `:2728`'s `format!("{stable_id} prompt")`. Only the bundle id changes (`hya/missing-root-def` → `hya/app-tests`), which nothing asserts on. **[inferred — not compiled]** |

So migrating `:2503` requires **two** helper changes, not one: a per-agent
`harness_access` and a per-agent prompt override. A minimal shape would be a
struct or a 5-tuple, e.g.
`&[(&str, AgentRole, &[&str], HarnessAccess, Option<&str>)]`, which touches all
11 existing call sites (4 in `spawn_admission.rs` + 7 `guidance_spawn_fixture`
call sites feeding `:1770` + 1 in `nested_spawn_tree.rs`). Given neither site is
reachable, deferring is defensible; the PRD already permits that.

---

## 6. R3 — duplication evidence (still byte-identical)

```
$ sed -n '43,78p' crates/hya-app/tests/spawn_admission.rs > a.txt
$ sed -n  '9,44p' crates/hya-app/tests/support/mod.rs      > b.txt
$ diff -u a.txt b.txt
--- a.txt
+++ b.txt
@@ -1,4 +1,4 @@
-fn verified_admission_test_runtime(
+pub fn test_runtime(
     tools: Arc<ToolRegistry>,
     agents: &[(&str, AgentRole, &[&str])],
 ) -> Arc<RuntimeRegistry> {

$ sed -n '47,78p' crates/hya-app/tests/spawn_admission.rs | md5sum
e527d528bd4df179e2ff83e25050134d  -
$ sed -n '13,44p' crates/hya-app/tests/support/mod.rs      | md5sum
e527d528bd4df179e2ff83e25050134d  -
```

**Bodies are byte-identical.** The only difference is the signature line
(`fn verified_admission_test_runtime(` vs `pub fn test_runtime(`). **[verified]**

`mod support;` is declared:

```
$ grep -n "support" crates/hya-app/tests/spawn_admission.rs
3:mod support;
(no other `support::` reference — remaining hits are the word "unsupported")
```

So `spawn_admission.rs` **declares `mod support;` at line 3 but never uses it**;
the module compiles only because `support/mod.rs:1` carries
`#![allow(dead_code, clippy::expect_used)]`. **[verified]**

Call sites to rewire when deleting the local helper (4):

```
crates/hya-app/tests/spawn_admission.rs:452   (admission_fixture_with_store_and_gate, :1431)
crates/hya-app/tests/spawn_admission.rs:1448  (admission_fixture_with_store_and_gate, second engine)
crates/hya-app/tests/spawn_admission.rs:1567  (queued_spawn_uses_parent_turn_binding_after_catalog_publication)
crates/hya-app/tests/spawn_admission.rs:1770  (guidance_spawn_fixture, :1748)
```
`guidance_spawn_fixture` fans out to 7 tests (`:1826, :1907, :2008, :2080, :2140,
:2220, :2312`). **[verified]**

---

## 7. R4 — `IdentityFakeProvider` and the `FakeProvider` blast radius

### 7.1 Location and shape **[verified]**

`crates/hya-app/tests/nested_spawn_tree.rs:36-67`. It is a thin wrapper:
delegates `id`, `capabilities`, `stream` to `self.inner: FakeProvider` and
overrides only:

```rust
55    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
56        Some(b"hya-test-nested-spawn-identity-v1".to_vec())
57    }
```

Used once, at `nested_spawn_tree.rs:70-72`. Nothing else references it. Moving
it to `tests/support/` is a pure relocation.

### 7.2 The hard constraint — the fail-closed assertion **[verified]**

PRD cites `crates/hya-app/src/runtime.rs:5016-5026`. The actual assertion block
is **`:5021-5031`** (the PRD is off by ~5 lines; `:5016-5019` is the preceding
route-ordering `assert_ne!`). Verbatim:

```rust
5021        let fake_router =
5022            Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(Vec::new()))));
5023        let fake_error = match AdmissionResolutionContext::capture(
5024            base,
5025            Arc::new(CategoryRegistry::default()),
5026            fake_router,
5027        ) {
5028            Ok(_) => panic!("a provider without configured identity must fail closed"),
5029            Err(error) => error,
5030        };
5031        assert_eq!(format!("{fake_error:?}"), "ProviderIdentityUnavailable");
```

Mechanism confirmed: `Provider::configured_identity_v1` defaults to `None`
(`crates/hya-provider/src/lib.rs:347-349`, doc comment: *"Providers without a
complete identity fail closed"*); `crates/hya-provider/src/fake.rs` has **no**
`configured_identity_v1` impl (`impl Provider for FakeProvider` at `:140`);
`ProviderRouter::configured_identities_v1`
(`crates/hya-provider/src/router.rs:26-36`) returns `None` if *any* member
returns `None` **or an empty identity**. So adding it to `FakeProvider` would
break this assertion. **Do not.**

### 7.3 `FakeProvider` usage census **[verified]**

```
$ grep -rn "FakeProvider" --include="*.rs" crates/ | wc -l
290 references across 103 files
```

Per crate (unique files):

| Crate | Files |
|---|---:|
| `hya-server` | 80 |
| `hya-core` | 14 |
| `hya-provider` | 6 |
| `hya-app` | 3 (`src/runtime.rs`, `tests/spawn_admission.rs`, `tests/nested_spawn_tree.rs`) |

Existing local wrappers that already do exactly what `IdentityFakeProvider` does
(evidence that this is the established pattern, and that a shared one is
warranted):

| Wrapper | Location | Identity bytes |
|---|---|---|
| `IdentityFakeProvider` | `hya-app/tests/nested_spawn_tree.rs:55` | `hya-test-nested-spawn-identity-v1` |
| `CountingProvider` | `hya-app/tests/spawn_admission.rs:147` | `hya-test-counting-provider-identity-v1` |
| `CaptureSystemsProvider` | `hya-app/tests/spawn_admission.rs:1527` | `hya-test-capture-systems-provider-identity-v1` |
| (two in-crate wrappers) | `hya-app/src/runtime.rs:4411`, `:6114`, `:8443` | delegate / local |

Moving `IdentityFakeProvider` into `tests/support/mod.rs` cannot disturb the 103
`FakeProvider` files, because `FakeProvider` itself is untouched. `support/mod.rs`
would need new imports (`hya_provider::{Capabilities, CompletionRequest,
EventStream, FakeProvider, Provider, ProviderError}`, `hya_proto::{MessageId,
ModelRef, SessionId}`, `async_trait`); `async-trait` is a normal dependency of
`hya-app` (`Cargo.toml:21`) and is already used from an integration test at
`nested_spawn_tree.rs:45`, so no manifest change is needed. **[verified]**
`support/mod.rs:1`'s `#![allow(dead_code, ...)]` already covers the fact that
`spawn_admission.rs` would compile it unused. **[verified]**

---

## 8. R5 — YAML hazard: the PRD's premise is wrong, but a real hazard exists

### 8.1 How the manifest is built **[verified]**

`crates/hya-app/tests/support/mod.rs:13-35` (identical in
`spawn_admission.rs:47-69`):

```rust
13    let mut manifest = String::from(
14        "api_version: hya.agent-bundle/v1\nkind: AgentBundle\nidentity:\n  id: hya/app-tests\n  version: 0.0.0\n  publisher: hya-tests\nagents:\n",
15    );
...
22        manifest.push_str(&format!(
23            "  - local_id: {stable_id}\n    stable_id: {stable_id}\n    role: {role}\n    prompt: prompts/{stable_id}.md\n    spawn_lifecycle: transient\n    harness_access: full\n"
24        ));
25        if !can_spawn.is_empty() {
26            manifest.push_str("    can_spawn: [");
27            manifest.push_str(&can_spawn.join(", "));
28            manifest.push_str("]\n");
29        }
```

Three unquoted interpolation points per agent: block-scalar `local_id`,
block-scalar `stable_id`, and a **flow sequence** `can_spawn: [a, b]`.

### 8.2 The parser is YAML 1.2-core, not 1.1 **[verified]**

`crates/hya-bundle/src/prepare.rs:505-509` parses with `serde_norway::from_slice`
(workspace dep `serde_norway = "0.9"`, resolved to 0.9.42 — a maintained
`serde_yaml` fork). In that version:

```rust
// serde_norway-0.9.42/src/de.rs:927-933
fn parse_bool(scalar: &str) -> Option<bool> {
    match scalar {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}
```

`no`/`on`/`y`/`off` are **not** booleans here. Furthermore the target fields are
`String` / `Vec<String>` (`crates/hya-bundle/src/source.rs:127-147`:
`local_id: String`, `stable_id: String`, `can_spawn: Vec<String>`), and
`deserialize_str` (`de.rs:1470-1491`) visits the raw scalar bytes without
consulting implicit tag resolution at all.

**Empirically confirmed** with a standalone probe (scratchpad crate, mirrors the
exact concatenation and the exact serde shapes):

| Input id | Result |
|---|---|
| `no` `No` `NO` `on` `On` `y` `Y` `off` `OFF` `yes` `true` `True` `false` `null` `Null` `~` `123` `0x1f` `1.5` `.inf` `.nan` `2026-08-06` `a#b` `a\tb` | **OK, exact round-trip** — the PRD's named hazards are all safe |
| `-` | ERR `block sequence entries are not allowed in this context` |
| `a: b` | ERR `mapping values are not allowed in this context` |
| `a]b` | ERR `did not find expected key` |
| `[a` | ERR `invalid type: sequence, expected a string` |
| `{a` | ERR `invalid type: map, expected a string` |
| `*a` | ERR `unknown anchor` |
| `!a` | ERR `... while scanning a tag` |
| `%a` `@a` `` `a `` `@build` | ERR `found character that cannot start any token` |
| `'a` `"a` | ERR `did not find expected key` |
| **`a,b`** | **OK but WRONG** — `can_spawn` silently becomes `["a", "b"]` |
| **`&a`** | **OK but WRONG** — parsed as an anchor; `local_id` becomes `""` |
| **`" a"` / `"a "`** | **OK but WRONG** — silently trimmed to `"a"` |
| **`""`** | **OK but WRONG** — `local_id` becomes `""`, `can_spawn` becomes `[]` |

So the real R5 statement is: **the hazard is not bareword booleans; it is (a) a
dozen YAML indicator characters that produce a confusing `InvalidManifest`
parse error, and (b) four inputs — `,`, leading `&`, surrounding whitespace,
empty string — that parse *successfully* into the wrong agent id.** (b) is
strictly worse than (a) and is what an assertion or proper quoting should target.

### 8.3 Does any current test pass such an id? **[verified]**

All ids reaching either helper today:

- `spawn_admission.rs:452` → `build`, `general`, `quick`, `plan`
- `spawn_admission.rs:1448` → `build`, `general`, `quick`
- `spawn_admission.rs:1567` → `build`, `general`, `quick`
- `spawn_admission.rs:1770` via `guidance_spawn_fixture` from `:1826, :1907,
  :2008, :2080, :2140, :2220, :2312` → `build`, `general`, `quick`, `plan`
- `nested_spawn_tree.rs:79-87` → `build`, `explore`, `general`, `plan`

Distinct set: **`build`, `explore`, `general`, `plan`, `quick`**. All plain
alphabetic barewords. **No current test passes a hazardous id.** R5 is purely a
forward-looking guard.

---

## 9. R6 — unbounded wait at `spawn_admission.rs:1681`

The offending await, in context (the supervisor was started 9 lines earlier at
`:1672`, so if it never replies the whole test binary hangs):

```rust
1671    let resident = ResidentSupervisor::start(engine.clone());
1672    spawn_team_supervisor(
1673        forward_rx,
1674        engine.clone(),
1675        base,
1676        router,
1677        Arc::new(CategoryRegistry::default()),
1678        resident,
1679    );
1680
1681    let outcomes = queued_spawn
1682        .await
1683        .expect("queued spawn task")
1684        .expect("queued foreground spawn");
```

`queued_spawn` is a `tokio::spawn(...)` handle created at `:1611`, so the
`.await` yields `Result<Result<Vec<_>, SpawnError>, JoinError>` — two `expect`s,
no bound.

Local idiom for exactly this shape, same file **[verified]**:

```rust
981     let outcomes = tokio::time::timeout(Duration::from_secs(5), spawn)
982         .await
983         .expect("foreground spawn timed out")
984         .expect("foreground spawn task panicked")
985         .expect("foreground spawn failed");
```

```rust
1169    let result = tokio::time::timeout(Duration::from_secs(5), spawn)
1170        .await
1171        .expect("foreground spawn timed out")
1172        .expect("spawn task panicked");
```

`Duration::from_secs(5)` is the file-wide convention: 25 `tokio::time::timeout`
call sites in `spawn_admission.rs`, of which 20 use `from_secs(5)`; the
`from_millis(100)`/`(150)` ones (`:913, :950, :975, :1089`) are deliberate
*negative* probes asserting a spawn is still pending. `nested_spawn_tree.rs`
uses the same 5 s budget (`:122`, `:182`). The fix that matches local idiom is a
`from_secs(5)` wrapper with a third `.expect`.

---

## 10. Things that make the work different from the PRD's assumption

1. **The reachable-and-migratable set is empty.** R2's "migrate the reachable
   ones" has no work item. The one CAN-REACH site is the one the PRD itself
   excludes, and it is additionally impossible to migrate with the current
   `BundleCatalog` API (§4.1). R2 should be closed as "surveyed; none".
2. **Only one of the two `harness_access` sites truly needs a signature change**,
   and it needs a *second* change too (per-agent prompt overrides) that the PRD
   does not mention (§5).
3. **R5's stated trigger (`no`, `on`, `y`, `off`) does not reproduce.** The
   parser is YAML 1.2-core and the fields deserialize as raw strings. The real
   silent-corruption inputs are `,`, leading `&`, surrounding whitespace, and the
   empty string (§8.2). An assertion written against the 1.1 bareword list would
   guard nothing.
4. **The spec's own trigger definition is too narrow** —
   `.trellis/spec/backend/task-tool.md:218-221` says "background", but foreground
   multi-member all-transient batches also route durable (§2, gate 4). Since this
   task is the one that owns that spec section, fixing it here is cheap.
5. **`spawn_admission.rs` declares `mod support;` but never uses it** — R3 is a
   4-call-site rewire plus deleting 36 lines, and removes a currently-dead
   module declaration (§6).
6. **PRD line reference drift**: the `FakeProvider` fail-closed assertion is at
   `runtime.rs:5021-5031`, not `5016-5026` (§7.2).

---

## Caveats / Not Found

- **Not compiled or executed.** No `cargo test`/`cargo check` was run against
  the repo (read-only task). All reachability conclusions are from source
  reading plus the crate dependency graph; the YAML table is from an executed
  standalone probe against `serde_norway 0.9.42`, not against
  `hya_bundle::prepare_builtins`, so `hya-bundle`'s *additional* validation
  (`AgentName::new`, strict-sort / dedup at `prepare.rs:131`, `:846-847`, hook
  and alias collision checks) may reject some ids my probe accepted. Ids my
  probe rejected will certainly still be rejected.
- **`:2761` migratability is inferred, not compiled** (§5). It looks mechanical
  but was not attempted.
- **Group A's 57 sites were classified by crate boundary, not individually
  read.** The boundary argument is decisive (`prepare_spawn_admission` is a
  private `fn` in `hya-app`; none of those crates depend on `hya-app`), so
  per-site reading would add nothing — but it does mean I have not characterised
  what each of the 32 `runtime_registry.rs` fixtures asserts.
- **Not surveyed**: whether any of the 99 sites could reach a *different*
  fail-closed fingerprint path (e.g. `hya-core/src/runtime_registry.rs:2481`,
  `:2662`, `:2876`). I checked those three specifically — all already use
  `from_verified_catalogs` — but did not audit `semantic_identity_v1` consumers
  exhaustively.
