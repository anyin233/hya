# Implementation plan — Bundle = one agent, isolated tool plane

Read `prd.md` and `design.md` first. This document is the ordered execution
checklist only. Do not re-plan here.

## Ground rules

- Work in a new git worktree off `main`. Branch: `feat/bundle-single-agent`.
- TDD gate per `AGENTS.md`: one atomic failing test first, confirm it fails for
  the expected reason, then the smallest change that makes it pass.
- Trellis scripts need Python 3.12+:
  `PY=~/.local/share/uv/python/cpython-3.13-linux-x86_64-gnu/bin/python3.13`.
  The system `python3` (3.10) fails on `.trellis/scripts/common/task_context.py:240`.
- The orchestrator dispatches each step to `plan-executor-heavy` or
  `plan-executor-bulk` per `CLAUDE.md`. The routing column below is the default;
  escalate on doubt.

### Verification commands

Per-step gate (fast):

```sh
cargo build --workspace
cargo test -p <touched-crate>
```

Phase gate (full, matches CI):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
```

Process E2E gate (Phase E only):

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
```

## Step 0 — Worktree and baseline

| Route | `plan-executor-bulk` |

- [ ] 0.1 Create the worktree off `main` and check out `feat/bundle-single-agent`.
- [ ] 0.2 Record a baseline: run the phase gate on the untouched tree and save
      the pass/fail list. Some existing failures may be pre-existing; you must
      know which before you start deleting tests.
- [ ] 0.3 Register the worktree path on the task with `task.py`.

**Rollback point R0.** Nothing changed yet.

## Phase A — Built-in agents leave the bundle system

Do this first. Until built-ins are independent, every bundle-format change
breaks the whole runtime and you cannot get a green build between steps.

### Step A1 — `BuiltinAgent` definitions

| Route | `plan-executor-heavy` |

- [ ] A1.1 Failing test: `hya-core` test asserting `BUILTIN_AGENTS` contains all
      15 ids with the roles, descriptions, and spawn lifecycles listed in
      `prd.md` R2.1. Source of truth for the current values is
      `bundles/builtin/hya-core-agents/bundle.yaml` and
      `bundles/builtin/hya-development/bundle.yaml`.
- [ ] A1.2 Create `crates/hya-core/src/builtin_agents/mod.rs` with
      `BuiltinAgent`, `SpawnScope`, `BuiltinModelPolicy`, and `BUILTIN_AGENTS`.
- [ ] A1.3 Copy the 12 prompt files from `bundles/builtin/*/prompts/` to
      `crates/hya-core/src/builtin_agents/prompts/` byte for byte. Wire them with
      `include_str!`. Do not reword any prompt in this task.
- [ ] A1.4 Mark `compaction`, `summary`, `title` as `system_reserved: true` with
      `SpawnScope::None`. Every other builtin gets `SpawnScope::AllOrdinary`.
- [ ] A1.5 Test: `SpawnScope::AllOrdinary` resolved against a catalog with zero
      installed bundles yields exactly the 12 non-reserved builtin ids.

**Gate:** `cargo test -p hya-core`.

### Step A2 — `AgentDefinition` and `AgentCatalog`

| Route | `plan-executor-heavy` |

- [ ] A2.1 Failing test: resolving `build` and an installed bundle agent through
      one `AgentCatalog::resolve` call, and asserting each reports the right
      `AgentOrigin`.
- [ ] A2.2 Add `AgentOrigin` and `AgentDefinition<'a>` (design 2.2). Put them in
      `hya-core` and re-export `AgentRole`, `SpawnLifecycle`, `ModelPolicy` from
      `hya-bundle`.
- [ ] A2.3 Add `crates/hya-core/src/agent_catalog.rs` with `resolve`,
      `resolve_entry`, `validate`, `spawnable`, `resolve_spawn`,
      `builtin_digest` (design 2.3).
- [ ] A2.4 `validate()` rejects an installed bundle whose agent id shadows a
      builtin id. Test it. (AC9)
- [ ] A2.5 `spawnable` skips unresolvable bundle `can_spawn` targets;
      `resolve_spawn` still errors on them. Test both halves. (AC11)

**Gate:** `cargo test -p hya-core`.

### Step A3 — Allow an empty bundle catalog

| Route | `plan-executor-heavy` |

- [ ] A3.1 Failing test: `BundleCatalog::from_prepared(&[])` succeeds.
- [ ] A3.2 Delete the `EmptyPreparedCatalog` rejection and its error variant.
- [ ] A3.3 Test: a runtime built with zero installed bundles resolves all 15
      builtin agents. (AC5)

**Gate:** `cargo test -p hya-bundle -p hya-core`.

### Step A4 — Swap the runtime over to `AgentCatalog`

| Route | `plan-executor-heavy` |

- [ ] A4.1 Change `RuntimeSnapshot.catalog` to `Arc<AgentCatalog>` and
      `publish_catalog` to match.
- [ ] A4.2 Convert `TurnBinding::{resolve_agent, resolve_requested_agent,
      resolve_spawn, spawnable_agents, agent_catalog}` to `AgentDefinition`.
- [ ] A4.3 Convert `hya-core/src/engine.rs` call sites (`agent_definition`,
      `agent_roster`, `effective_agent_for_binding`, summarize options).
- [ ] A4.4 Update `hya-core/src/test_support.rs` builders.

**Gate:** `cargo build --workspace && cargo test -p hya-core`.

### Step A5 — Delete the builtin bundle machinery

| Route | `plan-executor-heavy` |

- [ ] A5.1 Delete `crates/hya-app/build.rs`, its `[build-dependencies]` entry,
      `BUILTIN_BUNDLES`, `BUILTIN_BUNDLES_DIGEST`, `builtin_catalog()`,
      `builtin_catalog_from()`.
- [ ] A5.2 Delete `bundles/builtin/` and `hya_bundle::prepare_builtins`.
- [ ] A5.3 Rework `hya-app/src/runtime.rs:3986` to build an `AgentCatalog` from
      `BUILTIN_AGENTS` plus an empty `BundleCatalog`.
- [ ] A5.4 Rework `InstalledBundleRefresh` to hold no builtins.
- [ ] A5.5 Delete `crates/hya-app/tests/builtin_bundle_build.rs` and the
      builtin-tamper tests it owned. Replace with a test that a corrupt
      **installed** row is skipped and warned about, not a fatal error.
- [ ] A5.6 Delete `crates/hya-bundle/tests/builtin_source_parity.rs`.

**Gate:** phase gate. **Rollback point RA.** The tree builds, builtins work,
bundle format is still v1.

**Review gate GA — orchestrator reads before Phase B.** Confirm: no reference to
`bundles/builtin` remains; `rg 'builtin_catalog|prepare_builtins|BUILTIN_BUNDLES'`
returns nothing outside changelogs.

## Phase B — Single-agent bundle format

### Step B1 — Prepared model

| Route | `plan-executor-heavy` |

- [ ] B1.1 Failing test: `PreparedBundle` exposes `agent: PreparedAgent`. (AC3)
- [ ] B1.2 Apply the design 2.4 type changes: `agent` singular, `id` collapse,
      delete `HarnessAccess`, `PreparedBundleIndex.stable_agent_id`.
- [ ] B1.3 Bump `PREPARED_FORMAT_VERSION` to `2`.
- [ ] B1.4 Fix `prepared_bundle_is_canonical` and the canonical-ordering checks
      for a single agent.

### Step B2 — Source manifest

| Route | `plan-executor-heavy` |

- [ ] B2.1 Failing tests, one each: `agents:` rejected naming `agent:`;
      `api_version` rejected; `harness_access` rejected. (AC1, AC2)
- [ ] B2.2 Change `SourceManifest.agents: Vec<SourceAgent>` to
      `agent: SourceAgent`.
- [ ] B2.3 Add the `IgnoredAny` capture fields and
      `BundleError::RemovedManifestKey { key, guidance }` (design 2.5).
- [ ] B2.4 Keep the `bundle.hya.md` markdown-prompt path working. Simplify the
      "exactly one agent with no prompt field" rule now that it is structural.

### Step B3 — Prepare and catalog

| Route | `plan-executor-heavy` |

- [ ] B3.1 Rewrite `prepare_bundle` for one agent. Delete `prepare_builtins`.
- [ ] B3.2 Delete the `can_spawn` prepare-time validation near
      `prepare.rs:203` and the local-to-stable id rewrite near line 353. (R4.5)
- [ ] B3.3 Update `BundleCatalog::from_prepared` indexing for one agent.
- [ ] B3.4 Update `package.rs` inspection output to report one agent.

**Gate:** `cargo test -p hya-bundle`.

### Step B4 — Regenerate package fixtures

| Route | `plan-executor-bulk` |

- [ ] B4.1 Add a checked-in source tree for each of the five `.7z` fixtures under
      `crates/hya-bundle/tests/fixtures/sources/`.
- [ ] B4.2 Add an `xtask` subcommand that rebuilds the `.7z` fixtures from those
      sources, so the binaries stop being opaque.
- [ ] B4.3 Regenerate all five fixtures in the new format and update the tests in
      `hya-bundle/tests/{package_inspection,package_prepare,package_staging}.rs`
      and `hya-backend/tests/bundle_cli.rs`.

**Gate:** `cargo test -p hya-bundle -p hya-backend`.

### Step B5 — Rewrite the bundle test suite

| Route | `plan-executor-bulk` — escalate on any test whose intent is unclear |

- [ ] B5.1 Rewrite `hya-bundle/tests/{catalog, prepare, validation, markdown,
      docs_example}.rs` for the single-agent format.
- [ ] B5.2 Update `hya-core/tests/support/`, `hya-server/tests/support/`,
      `hya-core/src/test_support.rs`, `hya-app/tests/support/` builders.

**Gate:** phase gate. **Rollback point RB.**

**Review gate GB.** Confirm AC1, AC2, AC3 hold and no test asserts a multi-agent
bundle any more.

## Phase C — The clamped tool plane

This is the security-relevant phase. Route every step to `plan-executor-heavy`.

### Step C1 — Plane selection

| Route | `plan-executor-heavy` |

- [ ] C1.1 Failing test: a bundle agent's compiled view contains no MCP export,
      no plugin tool, and no project skill that a builtin agent sees in the same
      runtime. (AC6)
- [ ] C1.2 Add `AgentToolPlane { Full, InternalPublic }` in `hya-core`.
- [ ] C1.3 Rework `AgentResourcePolicy` to design 3.1: `plane`,
      `bundle: Option<String>`.
- [ ] C1.4 `TurnBinding::agent_resource_policy` derives the plane from
      `AgentOrigin`. Assert there is no code path that reads a plane from a
      manifest.

### Step C2 — Candidate collection

| Route | `plan-executor-heavy` |

- [ ] C2.1 Swap the `collect_harness_{tool,skill,mcp}_candidates` gate from
      `HarnessAccess` to `AgentToolPlane` per the design 3.2 table.
- [ ] C2.2 Guard `collect_bundle_{tool,skill,mcp}_candidates` behind
      `policy.bundle.is_some()`. A builtin agent has no bundle id and must not
      hard-error. Test a builtin turn end to end.
- [ ] C2.3 Confirm the change applies on the sidecar path
      (`compile_agent_resources_with_sidecar_tools`) and in
      `has_selected_bundle_sidecar_capability`. (R3.6)

### Step C3 — Close the escape hatches

| Route | `plan-executor-heavy` |

- [ ] C3.1 Failing test: a bundle `resource_view.allow` of `harness:mcp/...` and
      of `harness:skill/...` each fails the view with a plane-specific error.
      (AC7)
- [ ] C3.2 Add `BundleError::ResourceNotInPlane { reference, plane }` and raise
      it from `resolve_global_reference` (design 3.3).
- [ ] C3.3 Failing test: bundle A naming `bundle:B/tool/x` in `allow` is
      rejected. Then restrict the `allow`-driven re-resolution loop at
      `runtime_registry.rs:761` to the caller's own bundle id. This is a real
      cross-bundle leak that exists today — treat it as a fix, not a refactor.

### Step C4 — `basic_tools` invariant

| Route | `plan-executor-heavy` |

- [ ] C4.1 Add a debug assertion in `publish_candidate` that `basic_tools` is
      unchanged across publication.
- [ ] C4.2 Test: register an MCP source and a plugin source, then assert the
      `InternalPublic` plane for a bundle agent is byte-identical before and
      after. (design 3.4)
- [ ] C4.3 Document the `websearch` subtraction so the plane is not described as
      a fixed constant list.

**Gate:** phase gate. **Rollback point RC.**

**Review gate GC — mandatory adversarial review.** Before Phase D, have the
change reviewed specifically for plane escapes: any path that reaches
`snapshot.tools`, `self.skills()`, or MCP exports while
`plane == InternalPublic`.

## Phase D — Storage, CLI, and fingerprint

### Step D1 — Semantic fingerprint v2

| Route | `plan-executor-heavy` |

- [ ] D1.1 Add `AgentCatalog::builtin_digest()` as a `OnceLock` sha256 over the
      canonical serialisation of `BUILTIN_AGENTS`.
- [ ] D1.2 Recompose the fingerprint per design 6 and bump the domain tag to
      `hya.core.runtime-semantic-fingerprint/v2`.
- [ ] D1.3 Update `hya-core/tests/historical_agent_identity.rs`. Confirm no
      persisted session replay compares fingerprints across the upgrade
      boundary; if one does, report it before changing it.

### Step D2 — Installed-bundle registry

| Route | `plan-executor-heavy` |

- [ ] D2.1 Failing test: a stale row produces a named error, is skipped, and the
      next turn does not re-fail. (AC12)
- [ ] D2.2 Implement skip-and-warn plus generation advance in
      `InstalledBundleRefresh::refresh_if_changed` (design 5.1).
- [ ] D2.3 Bump the SQLite schema version in `hya-store/src/bundle_registry.rs`.
- [ ] D2.4 Replace `is_immutable_builtin` with the builtin-agent-id shadow check
      at install time, with an actionable message (design 5.2).

### Step D3 — CLI output

| Route | `plan-executor-bulk` |

- [ ] D3.1 `bundle_cmd.rs`: drop `print_builtin_info`; `list` and `info` show one
      agent; mark unreadable rows `unreadable (reinstall)`. (R5.3)
- [ ] D3.2 `agent_cmd.rs`: one list of builtins and bundle agents with an
      `origin` column.
- [ ] D3.3 Update `hya-backend/tests/bundle_cli.rs` and `cli_args.rs` tests.

**Gate:** phase gate. **Rollback point RD.**

## Phase E — Docs, ADR, and full verification

### Step E1 — Documentation

| Route | `plan-executor-bulk` |

- [ ] E1.1 Rewrite `docs/agent-bundle-authoring.md`: single-agent manifest, the
      clamped plane table, the removed keys and their errors, the `can_spawn`
      roster-vs-spawn split, and the **explicit statement that the clamp is not
      a sandbox** because a bundle agent may spawn a full-plane builtin.
- [ ] E1.2 Update `docs/examples/bundle.hya.md` and the three `bun-*` examples.
- [ ] E1.3 Update `CONTEXT.md`: "AgentBundle catalog" no longer covers builtins.
      Add "builtin agent registry" to the ubiquitous language.
- [ ] E1.4 Update `docs/configuration.md`, `docs/cli.md`, `docs/skills.md`,
      `docs/project-structure.md`, `docs/architecture/runtime.md` where they
      describe bundles or `harness_access`.
- [ ] E1.5 Add `docs/adr/0011-builtin-agent-registry-and-clamped-bundle-plane.md`.
- [ ] E1.6 Add the changelog entry and the "reinstall your bundles" release note.

### Step E2 — Full verification

| Route | `plan-executor-heavy` |

- [ ] E2.1 Phase gate, clean.
- [ ] E2.2 Process E2E gate.
- [ ] E2.3 `bun test test/real-backend.test.ts test/task-presentation.test.ts
      test/real-backend-agents.test.ts` after
      `cargo build -p hya-backend --bin hya-backend`.
- [ ] E2.4 Manual smoke: start the binary with zero installed bundles; confirm
      all builtin agents resolve. Install a single-agent bundle; confirm `build`
      can spawn it with no config edit. (AC8)
- [ ] E2.5 Walk every acceptance criterion in `prd.md` and record the test that
      proves it.

## Open item to resolve before Phase B

The PRD flags one assumption: whether the **internal** prepared-document
`format_version` survives. Phase A does not depend on it. Get the answer before
starting Step B1. If it is dropped, Step D2 must delete every installed row
rather than skip-and-warn, because a stale row would otherwise decode into the
wrong shape.

## Rollback points

| Id | State | Recover by |
| --- | --- | --- |
| R0 | Baseline | discard the worktree |
| RA | Builtins independent, format still v1 | `git reset --hard` to the RA commit |
| RB | Single-agent format landed | reset to RB; the clamp is not yet in |
| RC | Clamp landed | reset to RC; storage and CLI still old |
| RD | Storage and CLI landed | reset to RD; docs only remain |

Each rollback point is a commit on `feat/bundle-single-agent` that passes the
full phase gate. Do not carry a red phase gate past a rollback point.
