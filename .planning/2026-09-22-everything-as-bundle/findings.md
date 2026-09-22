# Findings: 2026-09-22 Everything as Bundle context

## Repository state

- 工作树 `/Users/saber/Projects/hya` 当前在 `main`，HEAD 与 `origin/main` 均为 `ac70f0e2`（goal mode 使用 file-backed store）。
- 最近 bundle 线：`609148d7`（bundle schema/process/MCP declarations）、`e8863c3d`（loop CLI + `hya/goal-loop` preset）、`23fa6f9d`（Claude adapter + bundle install/spawn）、`0f95d25c`（goal-loop e2e）、`b9200b2d`（bundle search）、`ac70f0e2`（goal store durability）。
- 当前工作区唯一显式未跟踪路径是 `crates/hya-plugin-compat/`；它只有一个 `adapter/node_modules/` 依赖树，没有源码文件，疑似旧 OpenCode/compat 工作树残留。没有触碰它。
- `.planning/.active_plan` 指向旧的 `2026-09-20-send-tool`，本次不覆盖；现上下文使用本目录显式路径。

## Current bundle architecture

- `hya-bundle` 已有 prepared format v2、strict source/package preparation、canonical digest、AgentBundle（单 agent）和 WorkflowBundle（单 workflow + 完整 agent closure）。
- `hya-app` 负责 first-party catalog embedding、installed bundle registry refresh、project bundles 和 runtime composition；`hya-store` 负责独立 SQLite bundle registry；`hya-backend bundle` 已支持 install/list/search/info/uninstall/schemas。
- `hya-core::AgentCatalog` 目前把两个 origin 合并为一个解析面：Rust 常量 built-in 与 installed bundle agent。`AgentOrigin::Builtin` 走 Full Harness plane，`AgentOrigin::Bundle` 走 InternalPublic/clamped plane。
- `RuntimeRegistry` 的 basic/full tool snapshots、resource view、namespace/mask/schema 表和 round-boundary refresh 已经是 bundle 化的主要运行时地基。
- first-party `hya/goal-loop` 是当前新式 preset bundle 案例；`hya/plan-impl-review` 是 first-party WorkflowBundle；Argus 是示例 WorkflowBundle。

## Existing design that points to the next phase

`.planning/2026-09-20-plugin-bundle-merge/dev_plan.md` 的 P8 明确列出：

1. P8.1 `AgentSetBundle` kind：`agents[] + channels[]`，无 workflow，并加入 prepare 校验。
2. P8.2 将 `BUILTIN_AGENTS` 迁移到嵌入 preset `hya/core-agents`，增加 preset plane，过渡后删除 legacy builtin。
3. P8.3 建立 `hya/base-tools` preset，把内置工具的可见性、别名和权限默认数据化；Rust 工具实现可继续保留 builtin。
4. P8.4 将 multiagent 拆成 subagent bundle 和 agent channel bundle 默认发布形态。

`design.md` 的统一模型写明：Plugin 是精简 bundle；完整 bundle 携带 tools/hooks/mcp/skills 与 agent 定义；安装入口最终统一到 `.hya/bundles`、`bundles:` config 和 `bundle install`；preset scope 低于 project/user。该设计还要求 preset agent 使用独立 plane，且保留 `read` 保护与事件溯源原则。

## Current boundary that must change

- `docs/agent-bundle-authoring.md` 开头仍明确写着 built-in agents “不是 bundles”，并指出它们编译于 `crates/hya-core/src/builtin_agents/`。
- `crates/hya-core/src/builtin_agents/mod.rs` 仍导出 `BUILTIN_AGENTS` 常量、编译 prompt、`SpawnScope`、reserved system agents 和 builtin digest。
- `crates/hya-core/src/agent_catalog.rs` 的 `AgentOrigin` 仍只有 `Builtin` 与 `Bundle`；built-in exact-id 优先，installed bundle shadow builtin 会硬失败 `BuiltinAgentIdShadowed`。
- `AgentResourcePolicy::for_origin` 当前只有 Full 与 InternalPublic 两种 plane；P8 设计需要增加 preset bundle 语义，而不能让 manifest 自己扩大权限。
- `hya-tool` 的 builtin registry 仍是 Rust 组装的工具实现与别名集合；P8.3 需要先把暴露策略与实现解耦，不能贸然把 Rust 工具本身搬进 JS sidecar。

## ZCode and historical traces

- `.zcode/plans/plan-sess_2649ae05-a9ab-414e-9f55-88843e893448.md` 只记录“6 个测试发现修复”计划（provider retry、exec JSONL、token ledger、`^parent`、`--pure`、SQLite materialization）。
- 对应 `.planning/2026-09-20-test-findings-fixes/progress.md` 已标记 `DONE`，六个提交均已经进入当前 Git 历史；该 zcode 计划不是 bundle 待办。
- Git history 中唯一直接的 ZCode 代码痕迹是 `97bc7f5f chore: ignore machine-local ZCode session state`，仅增加 `/.zcode/` 到 `.gitignore`。仓库正文没有 zcode runtime/API 实现。
- `.zcode/`、`.agent-docs/`、`.planning/`、`.autors/` 都是机器本地/忽略目录，不能当作产品源码或新的公开契约。

## Version and verification context

- `Cargo.toml` workspace version 当前为 `0.37.4`，root `CHANGELOG.md` 仍以 `0.37.0` 为最新文本；进入新功能实现必须先按 AGENTS.md 的 release/changelog 规则处理版本策略。
- 本次上下文读取没有修改 Rust/TypeScript/docs 产品文件，也没有运行完整验证门；下一次功能提交必须先写一个失败测试，再运行 touched-area gate，最后运行仓库要求的 fmt/clippy/workspace tests/backend build。

## Plugin integration boundary

Existing catalog generation refresh supports agentless prepared payloads, but Full-plane Skill candidate collection previously excluded every bundle Skill. A Plugin-specific visibility rule is needed for builtin agents to consume its published static Skills. Agent-bearing and unrecognized bundle sources must remain excluded from that exception. Process/MCP declarations remain metadata in this wave.

Plugin final boundary: runtime source kind is Bundle, distinct from process Plugin; the Full-plane exception checks both Bundle source identity and catalog-confirmed Plugin payload. Regression tests exclude AgentBundle/private and unknown bundle sources. Plugin preparation also runs validate_unsupported, preserving static extensions.rust rejection while retaining process declarations.
