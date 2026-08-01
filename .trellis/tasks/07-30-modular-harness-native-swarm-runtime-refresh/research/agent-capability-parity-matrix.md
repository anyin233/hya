# Native AgentBundle capability parity matrix

## Status, authority, and compatibility boundary

**Authority for this document:** direct reads of the isolated worktree
`codex/modular-harness-native-swarm-runtime-refresh` (uncommitted `0.34.8`
Commit 2 WIP). CodeGraph is navigation-only and non-authoritative. Workspace
`0.34.8` release metadata is applied.

This matrix has two layers that must not be conflated:

1. **Historical pre-cutover characterization** (committed `0.34.7` HEAD
   `064ede0b4fe4601b84ccbe912c75980449527d2c` and earlier). The “Historical
   parser / pre-cutover source” column freezes what the **deleted** old
   JSON/JSONC/Markdown agent parsers and `AgentCatalogPlane` path once did.
   That column is evidence of the break, not a support commitment.
2. **Current `0.34.8` Commit 2 WIP** (dirty worktree). The “Current native
   symbols + exact tests” and “0.34.8 WIP status” columns name what source
   and focused tests now implement. Local workspace / TUI / bin / zero-INET
   gates are **`LOCAL-GATES-GREEN`**; exact staging, commit, push, and remote
   CI remain **`PENDING-COMMIT-PUSH-REMOTE-CI`** (unclaimed).

Owner override (unchanged): no old-agent adapter, synthetic representation,
old-source catalog/CLI row, or agent-file execution fallback may ship; every
built-in agent is a native `AgentBundle`; stored `AgentName` values remain
decodable and replayable without implying a removed definition can execute.

Consult19–24 are controlling cutover rulings. Runtime consumes only embedded
prepared catalog bytes (decode once, fail-closed). Startup does not call the
package installer. Overall Commit 2 release envelope:
**`LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI`** (focused suites and
full local API/TUI/workspace/bin/zero-INET gates green; exact staging, commit,
push, and remote CI unclaimed).

## Evidence classes and status vocabulary

- **A_EFFECTIVE** — observable end-to-end effect that native Bundle must
  preserve (or intentionally replace under owner-approved fail-closed rules).
- **B_PARSED_IGNORED (historical)** — pre-cutover parsers accepted a value
  without delivering its advertised runtime effect. **Native v1 must not
  silently ignore:** `deny_unknown_fields` and typed `UNSUPPORTED_BUNDLE_FEATURE`
  / field rejection replace silent retain. Historical characterization remains
  in the historical column only.
- **C_DOC_ONLY (historical)** — documentation claimed behavior source did not
  provide. Bundle GA for that claim requires implementation + tests **or**
  explicit claim correction (see corrected `plan` description).

| Status token | Meaning |
| --- | --- |
| `HISTORICAL-PRE-CUTOVER` | Pre-delete characterization only. |
| `0.34.8-WIP FOCUSED-VERIFIED` | Direct source + named focused test(s) establish the cutover behavior. |
| `0.34.8-WIP TYPED-REJECT-VERIFIED` | Preparation/runtime typed rejection proven (no silent ignore). |
| `0.34.8-WIP SOURCE-PRESENT` | Implementation present in source; dedicated focused test not yet named or only partial. |
| `LOCAL-GATES-GREEN` | Full local API/TUI/workspace/bin/zero-INET verification green. |
| `PENDING-COMMIT-PUSH-REMOTE-CI` | Exact staging, commit, push, and remote CI unclaimed (no GA/remote success claim). |
| `NOT-AGENTBUNDLE-V1` | Global skill-catalog / non-bundle surface; **must not** be labeled a Bundle GA blocker. |
| `OUT-OF-SCOPE-0.34.8` | Deferred to a later patch or separate skill plane work. |

`GA-BLOCKING` is **removed** from rows whose native mapping is established by
direct source plus focused tests. It is **not** replaced by a claim of GA or
remote success; use `LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI` for
the Commit 2 release envelope.

## Field-level matrix

| Field path / behavior | Historical parser / pre-cutover source | Current native symbols + exact tests | Native Bundle mapping (0.34.8 WIP) | Class | 0.34.8 WIP status |
| --- | --- | --- | --- | --- | --- |
| Built-in catalog membership: `build`, `plan`, `general`, `explore`, `compaction`, `title`, `summary` | `compat::agent_catalog::NATIVE_AGENTS`, embedded prompt files (deleted) | `bundles/builtin/hya-core-agents/bundle.yaml`; `hya_bundle::prepare_builtins` / `PreparedCatalog::decode`; `hya_app::runtime::builtin_catalog`; tests: `hya-bundle` `builtin_source_parity`, `catalog`; app `builtin_catalog_initializes_once_and_shares_arc` | Prepared `hya/core-agents`, explicit stable IDs, `origin=builtin`, `immutable=true`; reserved system IDs `role=subagent` with empty ordinary inbound `can_spawn` | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED`; overall `LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI` |
| Single catalog authority: `RuntimeSnapshot` + `TurnBinding` | Split `AgentCatalogPlane` / live closures / hardcoded catalogs | `RuntimeSnapshot { catalog: Arc<BundleCatalog> }` in `hya_core::runtime_registry`; `RuntimeRegistry::new(tools, catalog)`; `TurnBinding` holds snapshot Arc; `hya_app::runtime` bootstrap passes `builtin_catalog()?` once | Exactly one `Arc<BundleCatalog>` born into registry, published in every immutable snapshot, captured by each `TurnBinding` | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` (`runtime_registry` / `runtime_turn_binding` / app bootstrap tests) |
| Embedded decode once / fail-closed bootstrap | N/A (compiled native list) | `PreparedCatalog::decode`; `builtin_catalog` `OnceLock`; tests: `zero_bundle_prepared_document_cannot_bootstrap_registry_catalog`, `corrupted_prepared_bytes_or_digest_fail_closed_with_decode_context`, `builtin_catalog_initializes_once_and_shares_arc` | Success and failure cached; corrupt/empty/tampered embedded artifact never silently substituted | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` |
| Agent public ID / stable `AgentName` | Map key / Markdown `name` → `AgentSpec.name` | `PreparedAgent.stable_id`; events/projection unchanged; tests: `historical_agent_name_replays_and_projects_exact_bytes_without_catalog_lookup`, `historical_agent_name_survives_read_only_fork_copy_without_catalog_lookup` | Public ID remains unqualified stable string; binding may resolve qualified internal refs | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` for identity/replay; full local API golden `LOCAL-GATES-GREEN`; `PENDING-COMMIT-PUSH-REMOTE-CI` |
| Source discovery / override order | Native defaults → global MD → workdir JSON/JSONC → workdir MD | **Deleted:** `agent_sources`, `agent_disk_sources`, `agent_catalog`, tracked `.hya/agents/*`; no production caller remains under `crates/hya-server/src/compat/` | Built-ins only from embedded prepared packages; no old discovery mapping | A_EFFECTIVE (deletion) | `0.34.8-WIP FOCUSED-VERIFIED` (git-deleted paths + absence of modules) |
| Catalog `description` | Accepted into list metadata | `PreparedAgent.description` / bundle YAML `description`; API metadata via `bound_agent_metadata`; focused tests: `compat_agent_metadata_api::{configured_default_agent_hya_main_is_first_among_agent_rows,compat_agent_route_includes_build_from_bound_catalog,compat_agent_routes_include_bound_catalog_agents,compat_agent_routes_ignore_project_legacy_agent_files,api_agent_metadata_from_bound_catalog_role_and_can_spawn}` | Catalog descriptor only | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED`; overall `LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI` |
| `role` / selector vs roster | Historical `mode` / `hidden` drove picker only | `AgentRole` on prepared agents; TUI `isTuiSelectableAgent` (`mode === "primary"` only); roster = caller `can_spawn`; tests: `packages/hya-tui-ts/test/agent-visibility.test.ts`; `role_selector_vs_can_spawn_roster::can_spawn_roster_includes_reachable_subagent_excludes_unlisted_main_and_system` | `role` = TUI selector only; `can_spawn` = sole ordinary reachability | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` |
| Reserved system agents `compaction`/`title`/`summary` | Accidental exact-name spawn possible | `role=subagent`, empty `can_spawn` inbound; fixed Harness callsites exact-resolve from same `TurnBinding`; tests: `fixed_system_agents::*`, `role_selector_vs_can_spawn_roster` | Exact lookup for fixed system ops; ordinary spawn → `AGENT_SPAWN_NOT_ALLOWED` / resolve_spawn err; missing def → `AGENT_DEFINITION_MISSING` | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` |
| Omitted agent vs explicit unknown | Unknown fell back to `general` under requested name | Omission → stable `general` (`TaskTool` / `resolve_requested_agent` / empty `subagent_type`); explicit unknown → `UNKNOWN_AGENT_ID`, no child; tests: `hya-tool/tests/task.rs::omitted_subagent_type_selects_general`; `spawn_admission::explicit_unknown_inline_target_creates_no_child`; `batch_invalid_member_with_guidance_has_zero_durable_side_effects` | Consult19 fail-closed split | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` |
| Missing historical definition continuation | Silent base/general risk | `CoreError::AgentDefinitionMissing`; tests: `historical_agent_continue_fails_definition_missing_before_provider_no_general_rewrite`, `forked_historical_session_continue_fails_definition_missing_before_provider`, `root_turn_missing_definition_fails_closed_without_general_fallback` | Replay/project exact bytes without catalog; continue fails typed before provider | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` |
| Model / category / reasoning | Parsers + `subagent_resolve` | Bundle model policy fields + session/switch precedence; spawn-member highest-to-lowest order proven by `hya_app` runtime unit test `resolve_spawn_member_model_precedence_highest_to_lowest`: spawn model > spawn category > inline model > Bundle model > inline category > Bundle category > base model; also `root_turn_bundle_precedence::*`, existing `model_selection` / category suites | Bundle policy + session model/event contract; spawn chain as ordered above | A_EFFECTIVE | Spawn-member precedence `0.34.8-WIP FOCUSED-VERIFIED`; full product surfaces `LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI` |
| `spawn_lifecycle` / resident | Entry `resident` + spawn flag | `SpawnLifecycle` on prepared agent; spawn ORs request; tests: spawn_admission resident paths, resident recovery suites | Lifecycle only when catalog agent is spawned; root TUI main remains Harness-owned | A_EFFECTIVE | Focused resident/admission paths verified; full matrix `LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI` |
| Prompt body / Bundle prompt vs Harness base | Disk body / config system | `agent_spec_for_binding` / `agent_with_guidance_layer`; tests: `root_turn_bundle_prompt_replaces_base_then_appends_skills_once`, `root_turn_prompt_none_preserves_composed_base_and_appends_skills_once`, server `compat_reference_guidance_api` | Bundle `Some` replaces only agent_base; `None` keeps Harness base | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` |
| Request-scoped guidance composition | Guidance concatenated into `AgentSpec.system_prompt` (broken with Bundle replace) | Server `session_agent_with_guidance` → `Option<Arc<str>>`; core `agent_with_guidance_layer`; child/resident carry Arc; tests: `compat_prompt_bundle_prompt_and_reference_guidance_parity`, `bundle_prompt_replaces_base_but_preserves_guidance_once_and_in_order`, `bundle_prompt_none_preserves_harness_base_then_guidance`, `guidance_captured_once_across_provider_rounds`, `spawn_admission::{transient_child_uses_triggering_turn_guidance_once_without_child_scan,resident_activations_reuse_in_process_triggering_guidance,nested_spawn_inherits_same_immutable_guidance,resident_guidance_is_ephemeral_not_persisted_in_events}` | Immutable request-scoped guidance; not in catalog/snapshot/wire | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED`; overall `LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI` |
| Request-scoped inline overlay | Inline fields via `subagent_resolve` (deleted) | `hya_app::runtime::resolve_spawn_member`: authorize base via `can_spawn` roster, then overlay name/prompt/model/category/resident; **typed reject** `inline.description` → `SpawnError::UnsupportedInlineAgentField { field: "description" }`; tests: `spawn_admission::inline_description_is_unsupported_before_admission_without_side_effects`, `explicit_unknown_inline_target_creates_no_child`, `authorized_inline_overlay_executes_without_catalog_entry`, `inline_child_spawns_through_its_authorized_base_roster` | Overlay never enters catalog/selector/roster | A_EFFECTIVE (+ typed reject for description) | Overlay + unknown + `inline.description` reject: `0.34.8-WIP FOCUSED-VERIFIED` |
| `none`/`basic`/`full` + allow/deny/alias/namespace compiled view | No effective per-agent resource parser | `TurnBinding::compile_agent_resources` → `Arc<CompiledResourceView>`; shared schema/skill/dispatch; tests: `agent_resource_view::{harness_access_filters_schema_dispatch_and_skill_prompt,canonical_allow_deny_and_alias_share_schema_and_dispatch,mcp_selected_public_name_dispatches_once_with_canonical_permission}`; many unit cases in `runtime_registry` tests | Cannot expand PermissionPlane/plugin authority | A_EFFECTIVE | `0.34.8-WIP FOCUSED-VERIFIED` |
| Historical B fields: `temperature`, `top_p`, `steps`/`maxSteps`, `request.headers`/`body`, non-reasoning options, `permission_overlay`, old permission spellings, `resource_profile`, executable tool/MCP/hook/JS/Rust without consumer | **Historical:** accepted and often ignored (`B_PARSED_IGNORED`) | **Native v1:** `#[serde(deny_unknown_fields)]` on bundle IR; preparation typed reject; tests: `hya-bundle/tests/validation.rs::invalid_schema_references_and_executable_features_fail_typed`, `unsupported_resource_profile_is_rejected_as_a_feature_not_ignored` | No silent ignore of unsupported AgentBundle v1 fields | Historical B → native typed reject | `0.34.8-WIP TYPED-REJECT-VERIFIED` (not silent ignore) |
| Built-in `plan` description “Disallows all edit tools” | **Historical C_DOC_ONLY** claim on native catalog metadata | Corrected: `bundles/builtin/hya-core-agents/bundle.yaml` `plan` description = `Plan mode. Planning-focused agent for designs and task breakdowns.`; no edit-deny claim; focused test: `hya-bundle` `builtin_source_parity::native_plan_description_does_not_claim_unimplemented_edit_prohibition` | Claim removed/corrected; not a silent permission overlay | Historical C closed by correction | `0.34.8-WIP FOCUSED-VERIFIED` |
| `color`, default_agent selection polish, full TUI/API goldens | Historical A paths | Partial wiring via bound metadata / TUI visibility helper | Presentation + default selection | A_EFFECTIVE | `LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI` for full product surfaces |
| Task tool transport (`task_id`, batch, background, members) | Runtime JSON (not file) | `TaskTool` / spawner / admission; extensive `spawn_admission` suite | Remains Harness ABI, not Bundle runner ABI | A_EFFECTIVE | Focused admission subset verified; full GA later |
| Prompt `@agent` attachment metadata | Recorded, no invoke | Unchanged event/projection path | Metadata only unless separate execution work | A_EFFECTIVE metadata | Unchanged; invocation not claimed |
| Global skill `name`/`description`/body/`disable` | `hya_tool::skill_catalog` discovery | Same skill plane; full harness_access agents still use workdir skill index | Not an AgentBundle v1 exclusive field set | A_EFFECTIVE (global skills) | Skill plane continues; Bundle cutover does not redefine global skills |
| Global skill `allowed-tools` / `model` / `license` | **Historical B_PARSED_IGNORED** on `SkillFrontmatter` in `skill_catalog.rs` — parsed, not enforced by `SkillPlane::require` | Still global SKILL.md parser behavior; **not** fields of AgentBundle v1 IR | **Do not** mislabel as Bundle GA blockers | Historical skill B | `NOT-AGENTBUNDLE-V1`; `OUT-OF-SCOPE-0.34.8` for Bundle GA |
| Legacy parser/discovery/`AgentCatalogPlane` / `.hya/agents` | Production authority pre-cutover | **Deleted** modules listed in git status (`agent_catalog`, `agent_disk_sources`, `agent_sources`, `agent_native_prompts`, `subagent_resolve`, …); tracked `.hya/agents/*.md` deleted | No dual catalog | A_EFFECTIVE (deletion) | `0.34.8-WIP FOCUSED-VERIFIED` |
| Docs example + authoring skill | N/A | `docs/examples/bundle.hya.md`; `docs/agent-bundle-authoring.md`; skill template `agent-bundle-authoring.md`; test: `hya-bundle/tests/docs_example.rs::docs_example_bundle_hya_md_prepares_deterministically` | Prepare-valid only; not runtime-scanned/installed/executed in 0.34.8 | A_EFFECTIVE (docs) | `0.34.8-WIP FOCUSED-VERIFIED` |

## Built-in native Bundle cutover contract (current WIP)

| Public ID | Role (prepared) | Ordinary `can_spawn` inbound | Prompt source | Native note |
| --- | --- | --- | --- | --- |
| `build` | main | listed by callers that include it | Bundle / base composition | Default main selection class |
| `plan` | main | listed by callers that include it | Bundle / base composition | Description corrected (no “disallows all edit tools”) |
| `general` | subagent | ordinary graph | Bundle / base | Omitted spawn target only |
| `explore` | subagent | ordinary graph when listed | Bundle prompt resource | Exact ID spawnable when allowed |
| `compaction` | subagent | **none** ordinary | Bundle prompt | Fixed compaction path exact lookup |
| `title` | subagent | **none** ordinary | Bundle prompt | Fixed auto-title exact lookup |
| `summary` | subagent | **none** ordinary | Bundle prompt | Fixed summarize exact lookup |

The native prepared catalog is the sole executable agent-definition authority in
this WIP. Rollback never reintroduces deleted parsers as a second authority.
Local API/TUI/workspace/bin/zero-INET gates are green
(`LOCAL-GATES-GREEN`); exact staging, commit, push, and remote CI remain
unclaimed (`PENDING-COMMIT-PUSH-REMOTE-CI`).

## Commit 2 focused evidence index (non-exhaustive)

| Contract | Primary symbols | Exact focused tests |
| --- | --- | --- |
| Single `Arc<BundleCatalog>` | `RuntimeSnapshot`, `RuntimeRegistry::new`, `TurnBinding` | `runtime_registry` suite; `runtime_turn_binding::admitted_turn_uses_one_binding_for_prompt_schema_skill_and_dispatch`; app `builtin_catalog_initializes_once_and_shares_arc` |
| Decode once / fail-closed | `PreparedCatalog::decode`, `builtin_catalog` | app unit tests in `runtime.rs` (empty/corrupt/digest/once) |
| Omitted vs unknown | `resolve_requested_agent`, task empty type, spawn resolve | `omitted_subagent_type_selects_general`; `explicit_unknown_inline_target_creates_no_child` |
| Role vs `can_spawn` | `spawnable_agents`, TUI `isTuiSelectableAgent` | `role_selector_vs_can_spawn_roster`; `agent-visibility.test.ts` |
| Reserved system exact lookup | title/summary/compaction engine paths | `fixed_system_agents.rs` (exact resolve + missing fail-closed + roster exclusion) |
| Historical identity / missing def | projection + continue | `historical_agent_identity.rs` |
| Inline overlay | `resolve_spawn_member` | `spawn_admission::inline_description_is_unsupported_before_admission_without_side_effects`, `authorized_inline_overlay_*`, `inline_child_spawns_*`, `explicit_unknown_inline_*` |
| Compiled resource view | `compile_agent_resources` | `agent_resource_view.rs` + `runtime_registry` alias/deny/allow units |
| Guidance composition | `agent_with_guidance_layer`, server guidance handoff | `compat_reference_guidance_api.rs`; `spawn_admission` guidance cases; `root_turn_bundle_precedence` |
| Docs example | prepare path | `docs_example.rs` |
| Legacy deletion | removed compat agent modules + `.hya/agents` | git `D` entries (no production module remains) |

## Release verification gate (`LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI`)

Local gates are green. Still unclaimed for release close-out:

1. Exact staging of the atomic Commit 2 change set.
2. Commit and push.
3. One green remote CI run.
4. No reintroduction of deleted agent-file authorities.
5. No GA or remote-success claim until the above land.

Focused verification of the cutover contracts in the evidence index is
**present** and is what removes row-level `GA-BLOCKING` language for those
contracts. Full local API/TUI/workspace/bin/zero-INET verification is
**`LOCAL-GATES-GREEN`**. Exact staging, commit, push, and remote CI remain
**`PENDING-COMMIT-PUSH-REMOTE-CI`**.

## Historical round-six uncertainty packet

```text
schema_version: 1
packet_id: UQ-AGENTBUNDLE-BOOTSTRAP-AND-UNKNOWN-ID-2026-07-31
task_id: modular-harness-native-swarm-runtime-refresh
prepared_by: fuji1 remote worker
authoritative_head_sha: 267bfc3c6c66e46fe8514e2e70657489f853b7f0
trigger_class:
  - architecture invariant conflict
  - two consequential designs lack an owner criterion
blocked_gate: native built-in AgentBundle cutover (not 0.34.3)
exact_questions:
  - Which single authority bootstraps the native built-in Bundle before the
    ordinary installed-bundle registry/runner is available: embedded,
    preinstalled, or registry-seeded?
  - Does the source-tested unknown subagent fallback to `general` remain an
    A-class native capability, or does the prior fail-closed missing-ID ruling
    intentionally break it at cutover?
why_uncertain:
  - Deleting old discovery in the same cutover requires a recovery-safe native
    source for built-ins, but the owner has deferred bootstrapping order.
  - Current HEAD explicitly tests unknown-name fallback, while the accepted
    stable-ID resolver design says missing IDs fail closed.
verified_facts:
  - Built-ins currently originate in agent_catalog::NATIVE_AGENTS and embedded
    prompt files.
  - resolve_subagent keeps the requested unknown AgentName and applies the
    `general` entry; an explicit test freezes that behavior.
  - SessionCreated/AgentSwitched/MemberSpawned/AgentRegistered persist public
    AgentName values, and Projection/fork replay those values.
  - The owner has forbidden old-agent adapters, synthetic representations, old
    file execution support, and dual runtime/catalog authorities.
candidates:
  - Embedded native built-in Bundle: simplest recovery and no install ordering;
    requires one compiler path shared with installed bundles.
  - Preinstalled/registry-seeded built-in Bundle: exercises distribution state
    earlier; startup and rollback depend on registry availability.
  - Unknown IDs preserve `general` fallback: behavioral parity, but conflicts
    with fail-closed identity and can hide typos.
  - Unknown IDs fail typed: safer stable-ID semantics, but an explicit breaking
    change to a source-tested behavior.
failure_impact:
  - No authoritative built-in source can make the agent runtime unbootable.
  - Dual fallback sources can drift and make rollback non-deterministic.
  - Silent unknown-ID fallback can execute the wrong prompt/model; abrupt
    rejection can break current task callers and historical continuation.
determinations_requested:
  - bootstrap authority and recovery/rollback rule
  - explicit resolution of unknown-ID parity versus fail-closed behavior
data_classification: public repository architecture; no secrets or private binary
redaction_attestation: only symbol names and behavior summaries are included
```

### Round-six disposition (retained) + Commit 2 WIP closure note

- **Bootstrap resolved:** build-time preparer, embedded package bytes,
  digest-bound index, immutable built-in origin, boot-without-install.
- **Old-source handling resolved:** no detector/adapter/migration; old files
  outside discovery (**WIP: production modules deleted**).
- **Unknown-new-spawn resolved by Consult19:** omission → `general`; explicit
  unknown typed fail (**WIP: focused tests green path present**).
- **Reserved reachability resolved by Consult20:** fixed system exact lookup;
  no ordinary `can_spawn` (**WIP: focused tests present**).
- **Commit 2 WIP additionally focused-verifies:** single `Arc` catalog +
  TurnBinding, decode-once fail-closed bootstrap, compiled resource view,
  request-scoped guidance, inline overlay authorization, docs example/skill,
  corrected `plan` description. **Release envelope:
  `LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI`.**
