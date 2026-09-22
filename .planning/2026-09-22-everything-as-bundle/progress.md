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
