# Implementation Plan: Remember Per-Agent Model Selections

## Execution rules

- Follow vertical TDD: one failing observable test, the smallest implementation, then the focused passing check.
- Do not alter user-owned `.agents/**` changes.
- Do not change Workflow model-route DSL/Event schemas, provider fallback semantics, generated SDK files, or Session Event replay.
- Reuse stable catalog Agent ids, the provider catalog, existing `DialogModel`, SDK transport, and Trellis/runtime control-port patterns.
- No live provider calls. All provider behavior uses existing fake/scripted routes.

## Slice 1 — owner-fenced durable preference rows

### RED

1. Add `crates/hya-store/tests/agent_model_preference.rs`.
2. Prove deterministic empty/list order, A/B isolation, upsert replacement,
   idempotent clear, bounded malformed-row rejection, and file-backed reopen.
3. Prove a stale runtime owner cannot mutate and concurrent valid transactions
   retain disjoint keys with the serialized last write for one key.

### GREEN

1. Add migration `0009_agent_model_preference.sql` with `agent_id`,
   `provider_id`, and `model_id` bounded non-empty columns.
2. Add a focused store module and exported typed row.
3. Add deterministic list plus owner-fenced transactional upsert/delete methods.

### Check

```sh
cargo test -p hya-store --test agent_model_preference
```

## Slice 2 — immutable bound view and one precedence resolver

### RED

1. Add focused `hya-core` tests for immutable snapshot capture and per-Agent
   lookup without map cloning.
2. Prove base < remembered < Agent category/direct < inline/request
   category/direct using the existing policy order.
3. Prove direct/category presence suppresses memory even when unresolvable,
   reasoning-only remains eligible, and stale models fall through.
4. Prove old `TurnBinding`s keep the captured map and new bindings see an
   infallibly published replacement.

### GREEN

1. Add `AgentModelPreferences`/snapshot in `hya-core` using retained Tokio watch
   state and `send_replace`.
2. Make `RuntimeRegistry::bind_turn` capture the immutable preference snapshot
   beside its existing runtime snapshot without turning preference writes into
   runtime-candidate refreshes.
3. Extend the existing category/model module with one pure resolver reused by
   all later execution paths.

### Check

```sh
cargo test -p hya-core agent_model_preference
cargo test -p hya-core runtime_registry
```

## Slice 3 — app-owned list/set control

### RED

1. Add `crates/hya-app/tests/agent_model_control.rs` through public app APIs.
2. Prove startup load, all `AgentCatalog::all()` rows including hidden agents,
   exact stable-id/model validation, configured lock, stale/dormant reporting,
   set/replace/clear, and restart.
3. Prove failed owner/store mutation does not publish and concurrent attached
   client changes preserve disjoint Agent preferences.

### GREEN

1. Add `PersistentAgentModelControl` in `hya-app` over `SessionStore` and the
   core publisher.
2. Load rows after runtime-owner claim and before work admission; fail startup
   with context on migration/query corruption.
3. Serialize bind/validate/commit/publish, commit SQLite first, then publish with
   infallible replacement.
4. Expose the same control from `BuiltSessionEngine` and in-process runtime
   composition.

### Check

```sh
cargo test -p hya-app --test agent_model_control
```

## Slice 4 — server control and root defaults

### RED

1. Add `crates/hya-server/tests/agent_model_preferences_api.rs` with a fake
   control: bootstrap/list state, every Agent class, exact catalog validation,
   set/replace/clear, configured conflict, unknown Agent, unavailable control,
   owner/store failure, and bounded structured errors.
2. Prove native, legacy, and V2 root creation use an eligible preference when
   the request omits model/category; explicit request model/category wins.
3. Add backend process coverage for exec/RPC/goal/Workflow CLI root creation,
   restart on the same `--db`, alternate-db isolation, and intentional in-memory
   non-durability. CLI overrides must not write preferences.

### GREEN

1. Add the dependency-inverted server `AgentModelControl` port, empty control,
   state installer, and OpenAPI registration.
2. Add `GET /tui/agent-models`, `PUT /tui/agent-models/:agent_id`, bootstrap
   `agentModels`, and `agentModelPreferences` capability.
3. Return normalized configured/settable/preference/stale/effective/source rows;
   do not change existing `/agent` or `/api/agent` semantics.
4. Use one app bind/default helper before `SessionCreated` in all root creation
   surfaces; leave fork/sync/recovery/resume models unchanged.

### Check

```sh
cargo test -p hya-server --test agent_model_preferences_api
cargo test -p hya-server --test tui_bootstrap_api
cargo test -p hya-backend agent_model_default
```

## Slice 5 — subagent and Workflow execution

### RED

1. Extend public spawn tests: unconfigured target uses remembered; Agent,
   inline, and spawn category/direct overrides retain exact order; A/B stay
   independent.
2. Prove durable admission captures one immutable snapshot, hashes only relevant
   targets, changes identity for a relevant preference, and ignores unrelated
   preference changes. Old accepted work and running residents remain pinned.
3. Prove unassigned Workflow workers and verifiers inherit remembered defaults,
   explicit Stage routes/results remain unchanged, and the request hash changes
   only for a relevant eligible preference.

### GREEN

1. Capture the preference snapshot in `AdmissionResolutionContext` and apply it
   immediately before existing `apply_spawn_model_policy`.
2. Add relevant-target preference identity to durable admission fingerprinting.
3. Carry the same snapshot through Workflow preflight; apply remembered defaults
   before existing Agent policy and explicit Stage assignments.
4. Add only relevant Workflow preferences to the request hash; do not change
   Workflow DSL, plan, route Event, reasoning, or fallback contracts.

### Check

```sh
cargo test -p hya-app spawn_model_precedence
cargo test -p hya-app agent_model_admission_fingerprint
cargo test -p hya-app workflow_model_preference
```

## Slice 6 — hidden system Agent execution

### RED

1. Capture fake provider requests for independent `title`, `summary`, and
   `compaction` preferences.
2. Prove both provider-native `compact_if_supported` and local summarizer paths
   use the resolved Compaction model.
3. Prove stale memory falls through, direct/category policy suppresses it, and
   normal turns plus threshold calculations retain the Session model.

### GREEN

1. Route fixed Agent option construction through the shared resolver.
2. Apply Title and Summary preferences to their actual provider requests.
3. Resolve Compaction once before branching and use it in both native/local
   calls without changing branch-selection behavior.

### Check

```sh
cargo test -p hya-core fixed_system_agent_model_preference
```

## Slice 7 — frontend wire decoding and synchronized state

### RED

1. Add `test/agent-models.test.ts` for exact row/capability decoding, all modes,
   stale catalog rows, configured rows, malformed/secret-like fields, and old
   backend fail-closed behavior.
2. Extend the SDK spine/state test for refresh, targeted effective lookup,
   set/clear, PUT-success-before-update, failure rollback/toast, and independent
   Agent rows.
3. Prove Session hydration and CLI seeds do not persist; normal picker/recent/
   favorite actions persist only an eligible current Agent's base model.

### GREEN

1. Add one hya-owned normalized Agent-model wire module.
2. Extend Sync/LocalProvider with targeted refresh/current/set/clear through the
   existing SDK generic fetch and directory header; add no second client/store.
3. Keep current-run Session/variant state separate. Replace synchronized rows
   only after backend success and preserve prior UI state on failure.

### Check

```sh
cd packages/hya-tui-ts
bun test test/agent-models.test.ts test/sdk-spine.test.ts
bun run typecheck
```

## Slice 8 — dedicated all-Agent model interaction

### RED

1. Add OpenTUI render/interaction tests at 80 columns for a refreshed target
   list grouped Main, Subagent, System.
2. Prove `/agents` and Tab cycling remain root-selectable role-`main` only.
3. Prove configured rows are visible/disabled, stale rows have text status, and
   eligible rows show the effective model.
4. Prove the reused model picker title names the target, selects its effective
   row, closes only after successful PUT, and the dedicated flow does not open
   a reasoning-variant picker.

### GREEN

1. Add `DialogAgentModel` for target selection without changing the current
   Agent.
2. Extend `DialogModel` with an optional target id and targeted LocalProvider
   calls; preserve normal current-Agent variant behavior.
3. Register `agent.model.list` and `/agent-models`; hide it when capability is
   absent and keep existing `/models`, `/agents`, recents, and favorites.

### Check

```sh
cd packages/hya-tui-ts
bun test test/agent-models.test.ts test/agent-visibility.test.ts
bun run typecheck
```

## Slice 9 — actual restart and runtime proof

1. Add a real-backend integration using an isolated file-backed database and
   fake provider catalog. Configure independent main, subagent, and hidden Agent
   models through the real control API; restart; assert normalized restored
   effective rows and an exact remembered subagent provider request.
2. Add a PTY interaction at 80 columns: open `Agent models`, choose a target,
   reuse the model dialog, restart, and observe restored target/model text.
3. Inject a stale remembered model and prove restart plus selection remain
   usable. Confirm hidden system Agents appear only in Agent models and never
   become `/agents` choices.

### Check

```sh
cargo build -p hya-backend --bin hya-backend -p hya-ts --bin hya-ts
cd packages/hya-tui-ts
bun test test/real-backend-agents.test.ts test/pty-smoke.test.ts
```

Keep exact provider-request assertion in the deterministic real-backend/E2E
harness, not transcript parsing. PTY proves the actual rendered interaction.

## Slice 10 — documentation and release metadata

1. Update `docs/tui-reference.md` with `/agent-models`, grouping,
   configured/dormant/stale behavior, and immediate backend persistence.
2. Update `docs/tui-keybindings.md` and `docs/cli.md` with the command.
3. Update `docs/configuration.md` with exact precedence, base-model-only writes,
   and the difference from TUI recents/favorites `model.json`.
4. Update `docs/architecture/storage.md`: auxiliary non-Event table, runtime
   owner fencing, selected-database scope, and in-memory non-durability.
5. Move root `CHANGELOG.md` to `docs/changes/CHANGELOG_0.36.9.md`.
6. Set workspace and TUI package version to `0.36.10`; write a newest-only root
   changelog for this feature.
7. Update lock/package metadata only through existing repository commands.

## Slice 11 — complete verification and review

Run focused RED/GREEN checks during each slice, then the touched-area gates:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --jobs 1 --exclude hya-e2e
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
cd packages/hya-tui-ts
bun run typecheck
bun test
bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

Rebuild `hya-backend` and `hya-ts`, then rerun the actual PTY smoke. Record exact
pass/fail counts and do not claim visual behavior beyond exercised frames.

Then:

1. Dispatch `trellis-check` with the curated task context and apply only
   source-verified findings.
2. Run `trellis-update-spec`; capture reusable durable control, binding, and TUI
   contracts in the relevant backend/frontend specs.
3. Re-run every gate affected by review/spec edits.
4. Use the commit skill. Stage only this feature, task artifacts,
   version/changelog/docs, and required spec changes; exclude existing unrelated
   `.agents/**` work.
5. Create one semantic feature commit and push it, as required by project policy.
6. Run authoritative `trellis-finish-work` to archive the task and record the
   journal, then push only resulting maintenance commits.

## Rollback points

- After Slice 1, the unused additive table can remain safely if later slices
  are reverted.
- The server's empty control and frontend capability gate preserve old-backend
  current-run model switching during partial rollback.
- Never delete preference rows or rewrite Session Events as rollback.
- An alternate database remains an intentional independent preference scope.
