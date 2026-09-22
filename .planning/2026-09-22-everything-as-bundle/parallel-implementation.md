# Parallel implementation: Plugin payload

Project: /Users/saber/Projects/hya
Phase: complete
Step: none
Outcome: complete
PLAN_ID: 2026-09-22-everything-as-bundle

User explicitly requests gpt-5.6-sol parallel subagents. Coordinator owns shared records, docs, version, final gates and serialized commits/push. Workers do not commit, bump versions or edit shared tracking files.

## Wave 1 contract

Implement `kind: Plugin` as the agreed agentless slim hyabundle payload. No Agent, agents, Workflow or channels fields. Preserve existing namespace/resource/schema/process-declaration validation, prepared v2 deterministic bytes, catalog and package lifecycle. Do not infer a fake Agent. Static skills must publish through existing immutable registry/round binding. Process/MCP execution is a separate remaining lifecycle contract; explicitly document actual support.

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| W1 | Plugin prepared/package/catalog | — | done | plugin_payload | Atomic red test then strict decode/prepare/package tests green |
| W2 | Agentless Plugin runtime static-resource refresh | — | done | plugin_runtime | Red integration scenario; install/refresh/uninstall and old binding semantics preserved |
| W3 | CLI/process E2E acceptance | — | done | plugin_acceptance | CLI agentless lifecycle plus static skill usable after refresh |
| W4 | Integrate, document, verify, commit/push | W1,W2,W3 | done | coordinator | Required Rust gates, backend build, process E2E pass; docs and version shipped |

Parallel workers share the working tree. Ownership: W1 only crates/hya-bundle; W2 only crates/hya-app and crates/hya-core; W3 only crates/hya-backend and crates/hya-e2e. All require backend/guides indexes and TDD. Report expected RED evidence before production edits. Child workers must not spawn further agents or alter branch/index.

Subsequent waves from strategy remain pending, not complete: preset provenance/core-agents, base-tools policy, subagent/channel policy, goal-loop and Claude integration. Do not conflate launch of a wave with completion of the overall roadmap.

## Integration findings

- Payload and CLI RED: `kind: Plugin` initially failed with `WrongKind`; now strict prepare and CLI lifecycle pass.
- Process RED: after successful install and root catalog refresh, builtin `build` could not load `plugin-help`; `collect_harness_skill_candidates` excluded all bundle Skills. Runtime worker is restricting the new Full-plane visibility exception to catalog-confirmed Plugin payloads.
- Valid Skill frontmatter is required at runtime; fixture now includes name and description.
- `/v1/skills` is not the bound runtime skill view; acceptance asserts actual skill tool output reaches the model follow-up request.
- Version for this atomic feature is 0.37.6; 0.37.5 changelog archived. Process/MCP automatic activation remains future work.

## Final integration corrections

- Added coordinator core regression `full_view_exposes_only_catalog_confirmed_plugin_bundle_skills`: RED on missing shared Skill, then GREEN after accepting catalog-confirmed Plugin Bundle sources in the Full view. Confirms third-party `acme` Plugin, qualified spelling, private AgentBundle exclusion, unknown source exclusion, and private agent isolation.
- Runtime source kind is `Bundle`, distinct from process `Plugin`; both source-kind admission and payload-kind check are needed.
- Focused P26 GREEN: active-server install/use/uninstall; fresh session cannot read removed Skill.
- Read-only cross-review caught missing `validate_unsupported`; worker added RED/GREEN regression for Plugin `extensions.rust` rejection.
- Updated exhaustive match in existing spawn admission test and test-only clippy expect allowances. Final full gates rerun after corrections.
- Markdown relative-link target check passed; matrix-check passed (49 scenarios, 10 retired ids).

## Verification results

- `cargo fmt --all --check`: PASS.
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS after final fixes.
- `cargo test --workspace --jobs 1 --exclude hya-e2e`: PASS, 1,737 passed / 3 ignored across 196 test groups (including doc tests).
- `cargo build -p hya-backend --bin hya-backend`: PASS after final fixes.
- Focused P26 and full `cargo test -p hya-e2e -- --test-threads=1`: PASS. All P01–P26 executables completed; no failures.

Wave 1 is complete. This record ships in the atomic Plugin feature commit; the coordinator commits and pushes after the gates. Subsequent bundle waves remain pending.
