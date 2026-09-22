Project: /Users/saber/Projects/hya
Phase: planning
Step: none
Outcome: active

# Task Plan: Everything as Bundle

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| 1 | 固化仓库、历史和 zcode 上下文 | — | done | coordinator | findings/progress 记录 HEAD、设计来源、工作区风险和历史计划状态 |
| 2 | 定义 AgentSetBundle 闭合契约 | 1 | done | coordinator | kind、manifest、prepared/catalog、registry 和解析边界有明确 schema 与失败条件 |
| 2.1 | 为 AgentSetBundle 添加一个原子失败测试 | 2 | done | coordinator | 测试先于实现，能证明当前缺失行为 |
| 2.2 | 实现 prepare/catalog/registry/runtime 的最小支持 | 2.1 | done | coordinator | AgentSetBundle 可被准备、合并、安装并解析，既有 AgentBundle/WorkflowBundle 不回归 |
| 2.3 | 补齐文档、版本和验证门 | 2.2 | done | coordinator | 文档覆盖介绍/用法/接口；fmt、clippy、workspace test、backend build、process E2E 通过 |
| 2.4 | Plugin（精简版 hyabundle）第一轮并行实现 | 2 | done | coordinator | 无 Agent payload、静态技能可见性、CLI/进程验收、文档和 0.37.6 版本；完整验证门通过 |
| 3 | 迁移 `hya/core-agents` preset | 2 | deferred | coordinator | built-in roster 由 preset 提供，legacy shim 的保留/删除有测试证据 |
| 4 | 数据化 `hya/base-tools` 暴露面 | 3 | deferred | coordinator | builtin 实现与 bundle 暴露策略分离，权限/别名语义可回放 |
| 5 | 拆分 subagent bundle 与 agent channel bundle | 2,3,4 | deferred | coordinator | multiagent 默认发布形态和 channel 合同有端到端证据 |

## Current next action

AgentSetBundle completed in `756aadd5`. Plugin Wave 1 is verified and ships as the next atomic commit; see [parallel implementation](parallel-implementation.md). Next implementation scope remains preset provenance and `hya/core-agents`, followed by `hya/base-tools`; automatic agentless process/MCP startup and channel contracts are not complete.
