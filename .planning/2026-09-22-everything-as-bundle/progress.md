# Progress: Everything as Bundle context

Project: /Users/saber/Projects/hya
PLAN_ID: 2026-09-22-everything-as-bundle
Phase: implementation
Outcome: complete

## Completed

- 读取并应用 `planning-with-files` shared workflow。
- 读取仓库 AGENTS.md 已给出的架构、TDD、文档、版本和验证约束。
- 核对 `main`/`origin/main`、近期 bundle 提交、bundle docs/ADR、`hya-bundle`/`hya-app`/`hya-core` 关键边界。
- 读取 `.zcode` 历史计划并与 `.planning/2026-09-20-test-findings-fixes/progress.md` 对账，确认其六项修复均已完成。
- 记录 P8.1–P8.4 以及当前 `builtin_agents`/`AgentCatalog`/tool plane 缺口。

## Changed paths

- Added ignored task records under `.planning/2026-09-22-everything-as-bundle/` only.
- 原有未跟踪 compat 目录已移至 `/private/tmp/hya-plugin-compat-stale-20260922-072407`，没有删除产品源码。

## Verification

- Read-only repository/history inspection complete.
- 第一条 prepare 测试先红后绿；新增空 roster、重复 agent、未知字段、权限字段、catalog 和 CLI 生命周期测试。
- 当前工作分支为 `codex/agent-set-bundle`，版本已升至 `0.37.5`，并补齐 AgentSetBundle 文档与 root/history changelog。
- bundle 专项测试已通过。默认沙箱的 workspace 测试曾因禁止监听 socket 让 3 个既有测试失败；提升权限重跑后 workspace 全部通过。
- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo build -p hya-backend --bin hya-backend` 全部通过。
- `cargo test -p hya-e2e -- --test-threads=1` 全部通过；P18 的 8 个慢测试最终通过（485.07s），P19–P25 也全部通过。

## Blockers / next handoff

- 验证门已清零；可提交并推送本原子变更。
- 本阶段不声称完成整个 P8：`channels[]`、`hya/core-agents` preset、`hya/base-tools` preset 仍是后续原子任务。

## P8.1 execution start

- Worktree audit passed: `main` and `origin/main` are both `ac70f0e2`; no staged or tracked diff remains.
- Moved only the untracked old compat `node_modules` cache to `/private/tmp/hya-plugin-compat-stale-20260922-072407`; no product file was deleted.
- Read backend/database/error/quality and cross-layer/code-reuse guides.
- Marked task 2/2.1 in progress. Added the first red test in `crates/hya-bundle/tests/prepare.rs` for a multi-agent `AgentSetBundle` payload.
- Red test result: `WrongKind { source_name: "agent-set", found: "AgentSetBundle" }`, as expected.
- 重新计算 digest 后的空 roster red test 暴露 prepared decoder 漏洞，已加入非空校验。

## 2026-09-22 — parallel Plugin implementation

User explicitly requested gpt-5.6-sol subagents. Three workers own payload, runtime, and CLI/process acceptance; coordinator integrates docs/version/gates. Plugin prepare and CLI RED tests failed with WrongKind before support; process acceptance revealed published Skills missing from Full-plane views. Runtime fix and scoped visibility regression coverage are being integrated. See parallel-implementation.md.

### Plugin Wave 1 verification complete

Version 0.37.6. Final fmt, full workspace clippy, workspace tests (1,737 passed / 3 ignored), local backend build, and full serial process E2E P01–P26 passed. Matrix-check and Markdown link target check passed. The atomic commit includes payload support, Plugin-only Full-plane static Skill visibility, isolation/lifecycle regression tests, CLI/P26 coverage, docs and archived changelog. Subsequent waves are still pending.

## Full-goal run started

User requests concurrent implementation of all planned bundle components with functional verification. Authoritative baseline afe29015 is clean/pushed. Active goal preserved at full scope. Started gpt-5.6-sol workers core_agents_preset, base_tools_preset, claude_bundle_import. Coordinator owns bundle runtime process/MCP composition and subsequent integration.

## Continued integration after Plugin wave

- B1/B2 worker implementations reached focused-test readiness; remain uncommitted pending integrated verification. Core worker now owns dynamic HookDispatcher composition and binding-based turn hooks. Base-tools worker owns strict agent-channel source/prepared schema and preset; runtime consumption remains pending.
- Coordinator process resource RED/GREEN evidence: `/private/tmp/hya-process-resources-red.log`, `hya-process-resources-green.log`, `hya-process-runtime-red.log`, `hya-process-runtime-green.log`. Added atomic installed-source cache, private materialization and native/MCP process retention. Agent-bearing process resources are not yet integrated.
- Claude worker now also owns backend Claude staging CLI to complete marketplace entry-point integration; lifecycle hooks require exact native semantics.
- Added P27 real backend test for native tool and MCP execution from packaged files, source/package removal and uninstall visibility. Build/functional run in progress.

## Integrated runtime checkpoint

- B4 owner-scoped process tools and grouped MCP selection now work. RED→GREEN tests in `/private/tmp/hya-private-process-{red,green}.log`, `hya-private-mcp-{red,green}.log`, and `hya-private-hooks-{red,green}.log`. Full core lib suite passed 147 tests.
- P27 passed 2 real backend tests: packaged native+MCP execution; deleted package; uninstall new-session visibility; private AgentSet tool/MCP dispatch and selected/unselected hooks.
- Installed refresh suite passed 7 tests including startup rollback and unchanged-source reuse. Schema P24 fixture upgraded from declarative placeholder to executable native/MCP source with actual `db://` read proof.
- B6 review identified live registry lookup and swallowed policy failures; worker is correcting these to captured policies and fail-closed behavior, then adding deeper functional cases. B4 session start/end and spawn observation still being pinned by core worker.
- B7 prompt fallback/override lifecycle integrated and focused-tested; Claude P29 process acceptance added. Version staged as 0.37.7; final gates and commits remain pending.

## Functional integration and final-gap audit

- P24 now executes the schema owner through `read db://…`; private process tools are wrapped with stable owner ids so URI dispatch stays scoped. P24 and P27 native/MCP/private hooks pass.
- P29 exposed standard Claude hooks-wrapper and native-to-Claude tool-name differences; adapter fixed both, and P29 passes after original source removal plus uninstall.
- New P27 JavaScript-only Plugin test first fails with unknown tool. Added implicit generation-owned Bun startup; adapter now accepts explicit plugin identity. Remaining initializer gating issue is assigned to Claude worker.
- MCP exported local ids containing `__` exposed a truncation bug; RED test added and export mapping now strips the exact server prefix.
- B6 frozen after actual refusals for all five capabilities, captured-policy refresh test, same-id no-trust test, and focused lint/test gates. Worker now closes dynamic `permission.ask` integration, which was declared but not wired.
- B1 uses explicit reserved/spawn policy, B4 lifecycle start/end uses a retained per-session dispatcher, B5 actual transient/resident app execution and replay pass. Core worker finalizes preset inventory and P28.
- First broad workspace Rust test run started; this is integration feedback until workers freeze. No unverified commit/push has been made.

## Final integration review

- Hook lifetime regression reproduced with `retained_bundle_hook_keeps_materialized_files_after_uninstall`: without owner retention the process loses packaged files after uninstall; retaining wrapper passes. Logs: `/private/tmp/hya-hook-files-{red,green}.log`.
- Agentless Bun extension initialization now loads explicit entrypoints without an Agent activation; bundled MCP gets private cwd/minimal env through `prepare_bundle`. Both adapters and MCP focused suites passed.
- Captured process `permission.ask` now participates in turn/shell authorization with private refs and explicit-deny precedence; focused tests and scoped clippy passed.
- Read-only B6 audit found resident lag recovery policy bypass, filtered steer cursor drift, and public API policy bypass. Core agent worker is adding regression tests and fixing these before final gates.
- Early broad test attempts were interrupted by concurrently edited compile seams and are not final verification evidence. Final gates will run after freeze.

## Contract audit follow-through

- P24/P27/P28/P29 passed together: six actual backend cases covering schema dispatch, native/MCP/JavaScript providers, private selection, subagent lifecycles, and Claude packaged hooks/Skills.
- Added P30/T2.25: installed channel-only policy denies child send with no parent payload delivery, then uninstall restores default delivery for a fresh session. Passed after isolating FakeLlm routes between stages (the initial shared root script let background follow-ups consume the next stage).
- Bundle workspace-adapter contributions were silently ignored. New native-process test failed as expected, explicit schema-boundary rejection added, then test passed.
- Bun adapter typecheck + 50 tests, Claude adapter typecheck + 47 tests passed. Matrix validation passed with 53 scenarios / 10 retired IDs.
- Bundle/tool preflight exposed stale documentation contracts. CLI and shipped authoring Skill now describe Plugin/AgentSet import, channel-only payloads, process/MCP support and trusted presets, while retaining activation-specific closure and wire details. The first-party publication assertion now describes shadowing before merged validation.

## Broad-gate findings

- Final Rust gate caught a moved-value diagnostic assertion in the mailbox regression test; changed the pattern to borrow the error message.
- The four existing round-rebind tests armed their fake refreshers before session creation/admission. New lifecycle/admission binding legitimately exercised those setup calls. Fixtures now attach the refresher only immediately before the measured turn; all original two-round behavior and exact refresh-count assertions remain, and all four pass.
- Inspection of the existing root round rebind path revealed a real integration gap: newly introduced bundle hook chains also need to change after a successful root round rebind. The hook worker is adding a two-round regression and updating captured hook/permission state; bound subagents retain their original binding. Documentation now accurately distinguishes root rounds from bound child activations.
- Resident delivery now requires both `resident_mail` and `follow_up`, since busy/idle arrival timing is not a durable event fact. The worker's original RED attempt was cancelled while waiting for the Cargo lock. Coordinator subsequently restored the previous OR predicate, confirmed the new regression fails (4 model calls vs expected 2), restored the conjunction, and is running GREEN. Logs `/private/tmp/hya-resident-capability-{red,green}.log`.

## Final frozen implementation

- Root hook-round regression passes: actual chat.params changes from temperature 0.1 to 0.9 after a successful source replacement between model rounds. Captured sidecar hooks are retained, hook/permission chains and channel policy swap at the same boundary, and steer cursor/queue/subscription remain intact. Existing round-rebind tests pass 4/4.
- Session channel policy updates at successful admission/rebinding so direct APIs agree with the current admitted snapshot; children continue using inherited bindings.
- Resident capability conjunction regression GREEN confirmed after coordinator reproduced old OR behavior.
- All workers are frozen. Final fmt/clippy/workspace tests/local executable build restarted against this integrated tree; full process suite will run against the rebuilt executable.

## Full-suite compatibility findings

- First complete process suite passed all 53 scenarios before the last root-round/background binding refinements. A final rerun against the rebuilt executable remains required.
- Full Rust no-fail-fast pass isolated failures to runtime_catalog_refresh and subagent integration targets; all other targets passed. Root/shell test fixtures now attach fake refreshers after setup, preserving one-admission assertions. Loop children needed an actual fix: capture inherited hooks and use bound prompt admission. Focused runtime_catalog_refresh now passes 4/4.
- Remaining sidecar regressions: the new general hook chain must not expand the legacy JS activation profile beyond event/tool-before/tool-after; the separate-engine direct-mail test must explicitly admit a binding before using captured channel policy. Workers are fixing these, plus the analogous background MCP completion binding path.

## Compatibility fixes frozen

- Restricted legacy JS sidecars at root/transient/resident extraction points; native process hooks retain their full surface. Both prior subagent regressions and the full 48-test subagent suite pass.
- Successful bound prompt admission publishes the validated channel policy only after message admission completes. Separate-engine direct-mail coverage now uses that real admission path.
- Background MCP completion retains its original binding and stable agent id. Eligibility follows `ToolPermission::Mcp`, including bundled names; all three MCP background tests pass with exact refresh-count checks.
- All workers frozen; final fmt, full strict clippy, full workspace tests, backend build and serial process E2E restarted. Product docs describe background completion binding semantics.

## Final gate feedback

- Full strict workspace clippy and fmt passed. Full Rust suite passed 1784 tests, 0 failures, 3 ignored; local backend build passed. Matrix remains 53 scenarios / 10 retired IDs. Version/archive and documentation links checked.
- Rebuilt-process E2E found P18 Skill error classification regression: creating `Some(empty HookChain)` for every root turn routed unchanged `ToolError::Input` through hook-result normalization and converted it to `Other`. Worker reproduced the failure and is replacing the empty dispatcher with an optional, still-replaceable activation context. Final gates will be repeated after this correction, including initially absent hooks becoming active at the next root round.

- P18 fix frozen: activation context supports `None → Some → None`, and unchanged after-hook results preserve their original typed errors. A real two-round model test now begins without hooks and observes the newly installed second-round hook; a unit regression covers removal back to None. Rebuilt-backend exact P18 test passes, as do MCP background 3/3 and focused clippy. Final full gates restarted in `/private/tmp/hya-bundle-*-certified.log`.

- That rerun passed strict clippy but exposed stack overflow in seven child-heavy Rust test targets after the new generic async hook wrapper. P18's prior full run completed with only the already-fixed Skill failure. Worker is reducing async future stack pressure and reproducing the child path with default stack settings; no stack-limit override is accepted as verification. Full gates remain pending.

- Stack overflow fixed by directly scoping a boxed future in both activation hook entrypoints, avoiding nested inline generic turn futures. Default-stack checks pass for all seven failing paths, including full subagent 48/48 and workflow 19/19. No stack-limit/environment override. Final full gates restarted in `/private/tmp/hya-bundle-*-releasecheck.log`.

- Final frozen implementation passes fmt, strict full-workspace clippy and the full Rust suite: 1785 passed, 0 failed, 3 ignored. Backend rebuild and serial process E2E are the remaining running gates.

## Requirement-to-evidence map

| Scope | Functional evidence |
| --- | --- |
| B1 core-agents | `agent_catalog`, `fixed_system_agents`, `preset_inventory`: prepared source parity, reserved-id isolation, read-only inventory |
| B2 base-tools | `base_tools_preset`, tool registry and permission suites: canonical names, aliases, visibility and permission parity |
| B3 Claude import | Adapter 47 tests, backend `bundle_cli`, P29: multi-agent/agentless conversion, marketplace staging, packaged hooks and Skills after source removal |
| B4 process/MCP | `installed_bundle_refresh`, bundle runtime tests, P24/P27: actual tools/schema reads, owner scope, rollback, retained files, JavaScript-only Plugin |
| B5 subagent bundle | `first_party_subagent_runtime`, subagent 48 tests, P28: transient report, resident mailbox, nested spawn admission and replay |
| B6 agent channel bundle | `agent_channels`, `channel_policy`, mailbox/report/steer/resident recovery, P30: strict schema, restrictive grants, captured policies, durable denied-mail consumption and uninstall restoration |
| B7 goal-loop | `goal_loop`, `loop_mode`, `bundle_cli`, installed override tests, P15/P16: prompt/hook fallback, installed override removal, independent evaluation and engine-owned stop |
| Cross-cutting | `round_rebind_hooks`, runtime refresh, P18: next-round hook install/removal, bound child/background completion snapshots, original typed tool errors preserved |

Process references above identify acceptance coverage; the final full process rerun must pass before marking B8 complete.

## Final acceptance — 0.37.7

- `cargo fmt --all --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace --jobs 1 --exclude hya-e2e --no-fail-fast`: 1785 passed, 0 failed, 3 ignored.
- `cargo build -p hya-backend --bin hya-backend`: passed.
- `cargo test -p hya-e2e -- --test-threads=1`: 53 passed, 0 failed; complete P01–P30 suite, including P18 (485-second expected timeout/recovery coverage).
- Bun adapter typecheck/test: 50 passed; Claude adapter typecheck/test: 47 passed. Neither adapter changed after those gates.
- Matrix validation: 53 scenarios / 10 retired IDs. Version/Cargo.lock/root changelog agree on 0.37.7; 0.37.6 changelog archive exactly preserves the preceding root. Documentation relative links and diff whitespace checks pass.
- Final Rust/build/process logs: `/private/tmp/hya-bundle-*-releasecheck.log`. All workers frozen; no remaining implementation blockers. Coordinator commits this verified feature and checks pushed branch equality before reporting completion.
- Final staged-file whitespace check found one extra trailing blank line in `base-tools/exposure.yaml`; removed without changing YAML values. Re-ran both embedded-preset/permission parity tests and rebuilt the backend successfully; final full-diff whitespace check passes. No runtime source changed after the full gates.
