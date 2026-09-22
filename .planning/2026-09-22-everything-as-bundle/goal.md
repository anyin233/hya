# Everything as Bundle: full implementation goal

Project: /Users/saber/Projects/hya
PLAN_ID: 2026-09-22-everything-as-bundle
Status: complete

## Current objective

Bundleize all components in the agreed zcode plan and prove functional behavior. The original AgentSetBundle and agentless Plugin milestones are completed foundations, not completion of this goal.

## Required scope and acceptance

- B1 `hya/core-agents`: prepared trusted preset, reserved-agent privilege isolation, runtime catalog parity.
- B2 `hya/base-tools`: data-owned tool exposure, aliases, permissions, protected names; existing native implementations remain Rust.
- B3 Claude import: complete source closure, agentless Plugin / multi-agent AgentSetBundle, commands/skills/hooks/MCP, local and marketplace import with actual staged runtime execution.
- B4 process/MCP: prepare before atomic publication, pinned lifetime, rollback on failure, owner-scoped resources, actual tool/hook/MCP execution after source removal.
- B5 subagent bundle: transient/resident definitions and nested spawn, reports, mailbox and durable replay.
- B6 agent channel bundle: validated declarative policy consumed by routing, scoped send/report/steer/follow-up and retention; channel IDs remain event-minted.
- B7 `hya/goal-loop`: bundle-supplied planning/evaluation resources, process hook then prompt then builtin fallback, independent lifecycle; engine retains stop authority.
- B8 full Rust, build, process E2E and touched Bun verification; documented interfaces/usage; versioned atomic commits pushed; requirement-by-requirement audit.

## Historical milestone context

The section below describes the completed initial 0.37.5 milestone only; its non-goals no longer constrain the full continuation.

# Everything as Bundle：上下文基线与 P8 起点

Project: /Users/saber/Projects/hya
Date: 2026-09-22

## Goal

在现有 AgentBundle、WorkflowBundle、bundle registry/catalog、运行时资源视图和 first-party preset 基础上，继续推进 hya 的 “everything as bundle” 架构。本阶段实现 P8.1 的第一块可验证合同：可安装、可解析、可搜索的 `AgentSetBundle`，并为后续声明式 channels 与 preset 迁移保留边界。

## Scope

- 为一个 prepared payload 支持多个 agent 的 manifest、摘要、catalog、安装和 CLI 生命周期。
- 固化当前仓库 HEAD、bundle 相关历史、设计文档、zcode 历史计划和工作区状态。
- 明确 P8.1–P8.4 的依赖关系与当前实现缺口。

## Non-goals for this phase

- 不删除或恢复 `crates/hya-plugin-compat/`。
- 不迁移 built-in agents、不改变工具 registry。
- 不把运行时产生的 channel 事件改造成声明式 `channels[]` 合同；该合同仍是后续原子工作。

## Acceptance

- `AgentSetBundle` 可通过 prepare、prepared decode、catalog、bundle CLI 生命周期测试。
- 文档、版本、变更记录与验证记录随同原子变更维护。

## Unresolved decisions

1. `AgentSetBundle` 当前只实现 `agents[]`；声明式 `channels[]` 需要单独定义资源所有权、生命周期和事件映射。
2. `hya/core-agents` 迁移时 reserved system agents（compaction/summary/title）是否进入 preset，还是保留最小 legacy runtime shim。
3. `hya/base-tools` 的 preset 是只描述暴露面/别名/默认权限，还是同时承载工具实现资源。
4. 本原子功能使用 workspace `0.37.5`，根 CHANGELOG 保留最新版本，旧 `0.37.0` 文本移入 `docs/changes/CHANGELOG_0.37.0.md`；发布 tag 仍留待正式发布流程。
