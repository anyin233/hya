# 0.36.39

## Subagent orchestration redesign: unified episodic-resident lifecycle, channel communication plane, workflow on one substrate (core, tool, app, proto)

- **Unified lifecycle (ADR-0015)**: every subagent spawns as a resident actor
  through one admission path; `task` is always non-blocking and returns the
  child's handle immediately. Completion is the `report` tool, gated on a
  drained inbox and archived children; the engine then writes a state-only
  six-section handoff (degraded deterministically on summarizer failure),
  delivers the report to the parent over the pair DM channel, and archives the
  agent (roster exit, claim release, archive history). A downward `dm` revives
  an archived direct child from its handoff. Turn errors synthesize a failure
  report; parents can `kill` a stuck child; subagent depth is hardcoded to two
  layers and the orchestration tools (`task`, `list_agents`, `workflow`,
  `search_agent`, `kill`) are hidden at depth 2.
- **Channel plane (ADR-0016)**: registration mints a unit group channel
  (`announce-{8}`, leader-only posting, no member list) and a persistent
  parent-child DM channel (`DM-{8}`, the revival address). New tools `dm`,
  `broadcast`, `list_channel`, and `search_agent` replace `send`, `announce`,
  `roster`, `channels`, `join`, and `leave`; addressing is vertical only.
- **Workflow on the unified substrate (ADR-0017)**: every stage member runs as
  a parked resident actor; the transient team join is retired from the
  stage path.
- **Breaking by mandate**: `task_id` resume, `background`/`resident` task
  fields, sibling mail, named user channels, and the `subagents.max_depth`
  config key are removed with no compatibility shim. Known follow-up: the
  workflow model-routing process e2e (p19) times out on run completion and
  channel-plane e2e choreography (p16) needs re-adding.
