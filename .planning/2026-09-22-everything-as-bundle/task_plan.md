Project: /Users/saber/Projects/hya
Phase: verification
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
| 3 | 迁移 `hya/core-agents` preset | 2 | deferred | coordinator | built-in roster 由 preset 提供，legacy shim 的保留/删除有测试证据 |
| 4 | 数据化 `hya/base-tools` 暴露面 | 3 | deferred | coordinator | builtin 实现与 bundle 暴露策略分离，权限/别名语义可回放 |
| 5 | 拆分 subagent bundle 与 agent-channel bundle | 2,3,4 | deferred | coordinator | multiagent 默认发布形态和 channel 合同有端到端证据 |

## Current next action

完成 `AgentSetBundle` 的静态验证和验证门；随后只在所有门通过后提交并推送本原子变更。`channels[]`、core-agents 和 base-tools 保留为后续任务。
