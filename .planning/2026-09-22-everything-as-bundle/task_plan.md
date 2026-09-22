Project: /Users/saber/Projects/hya
Phase: complete
Step: deliver
Outcome: success

# Task Plan: Everything as Bundle

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| 1 | 固化仓库、历史和 zcode 上下文 | — | done | coordinator | findings/progress 记录 HEAD、设计来源、工作区风险和历史计划状态 |
| 2 | 定义 AgentSetBundle 闭合契约 | 1 | done | coordinator | kind、manifest、prepared/catalog、registry 和解析边界有明确 schema 与失败条件 |
| 2.1 | 为 AgentSetBundle 添加一个原子失败测试 | 2 | done | coordinator | 测试先于实现，能证明当前缺失行为 |
| 2.2 | 实现 prepare/catalog/registry/runtime 的最小支持 | 2.1 | done | coordinator | AgentSetBundle 可被准备、合并、安装并解析，既有 AgentBundle/WorkflowBundle 不回归 |
| 2.3 | 补齐文档、版本和验证门 | 2.2 | done | coordinator | 文档覆盖介绍/用法/接口；fmt、clippy、workspace test、backend build、process E2E 通过 |
| 2.4 | Plugin（精简版 hyabundle）第一轮并行实现 | 2 | done | coordinator | 无 Agent payload、静态技能可见性、CLI/进程验收、文档和 0.37.6 版本；完整验证门通过 |
| 3 | 迁移 `hya/core-agents` preset | 2 | done | coordinator | built-in roster 由 preset 提供，legacy shim 的保留/删除有测试证据 |
| 4 | 数据化 `hya/base-tools` 暴露面 | 3 | done | coordinator | builtin 实现与 bundle 暴露策略分离，权限/别名语义可回放 |
| 5 | 拆分 subagent bundle 与 agent channel bundle | 2,3,4 | done | coordinator | multiagent 默认发布形态和 channel 合同有端到端证据 |

## Current next action

B1–B8 implementation, documentation and all verification gates are complete. The coordinator is delivering the verified 0.37.7 atomic change on `codex/agent-set-bundle`; final user-facing completion requires the pushed branch to match local HEAD. See progress.md for acceptance evidence.

## Full-goal continuation (after afe29015)

Objective: bundleize every planned component and prove functional behavior. Prior Plugin wave is progress, not full-goal completion. Baseline clean and pushed at afe29015. Use this explicit PLAN_ID; no implicit workflow session selection.

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| B1 | Trusted hya/core-agents prepared preset and catalog switch | 2.4 | done | core_agents_preset | Real embedded AgentSetBundle source replaces Rust roster; parity and reserved privilege isolation |
| B2 | hya/base-tools exposure/alias/permission policy preset | 2.4 | done | base_tools_preset | Runtime registry derives policy from Plugin resources; full tool/schema/permission parity |
| B3 | Claude standard bundle import closure | 2.4 | done | claude_bundle_import | Agentless Plugin / AgentSet conversion, commands/skills/agents/hooks/MCP metadata; strict install validation |
| B4 | Bundle process and MCP runtime lifecycle | 2.4 | done | coordinator | Prepare/start/publish/remove with immutable bindings, failure rollback, actual tool/hook/MCP process tests |
| B5 | subagent bundle source and spawn/replay integration | B1,B2 | done | coordinator | Transient/resident definitions supplied by bundle, allowed/denied nested spawn, mailbox/report/replay tests |
| B6 | agent channel bundle policy and runtime consumption | B5 | done | base_tools_preset + coordinator | Declarative topology/policy, event-minted IDs, scoped routing/send/report/steer/retention and replay |
| B7 | hya/goal-loop intelligence bundle | B4 | done | coordinator | Contract/rubric/verifier/planner resources and hooks, install/uninstall fallback; engine stop authority unchanged |
| B8 | Cross-component acceptance and final audit | B1,B2,B3,B4,B5,B6,B7 | done | coordinator | Full Rust/build/process gates, Bun gates if touched, all docs/version/atomic commits pushed; requirement-by-requirement proof |

Each worker owns disjoint paths specified in its assignment. Shared docs/version/lock/plans/Git index are coordinator-owned. Record incomplete contracts honestly; do not mark whole goal complete for an intermediate wave.
