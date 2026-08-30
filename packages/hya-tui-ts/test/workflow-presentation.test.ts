import { expect, test } from "bun:test"

import { parseWorkflowProjection, presentWorkflow } from "../src/hya/workflow-presentation"

const identity = {
  source: "bundle:acme/release/workflow/release",
  name: "release",
  revision: "ab".repeat(32),
}

const member = (id: string) => ({ member: id, role: "worker", iteration: 0 })
const sessionMember = (id: string, status: "spawning" | "running", work = "") => ({ member: id, status, work })

const stage = (
  id: string,
  status: "pending" | "running" | "completed" | "failed" | "cancelled" | "skipped",
  level: number,
  members: ReturnType<typeof member>[] = [],
  title?: string,
) => ({ id, title, agent: `${id}-agent`, mode: "once", level, status, members })

test("Workflow presentation covers no selection and every durable terminal state", () => {
  expect(presentWorkflow(undefined)).toMatchObject({ state: "none", name: "none", tone: "muted" })
  expect(presentWorkflow({ selection: identity, availability: "available" })).toMatchObject({
    state: "ready",
    name: "release",
    revision: "abababab",
    tone: "info",
  })

  for (const [status, tone] of [
    ["completed", "success"],
    ["failed", "error"],
    ["cancelled", "warning"],
    ["interrupted", "warning"],
  ] as const) {
    const view = presentWorkflow({
      selection: identity,
      availability: "available",
      run: {
        id: "run-1",
        workflow: identity,
        request_hash: "hash",
        owner: "owner",
        status,
        stages: [stage("finish", status === "completed" ? "completed" : status === "failed" ? "failed" : "cancelled", 0)],
      },
    })
    expect(view.state).toBe(status)
    expect(view.tone).toBe(tone)
  }

  expect(presentWorkflow({ selection: identity, availability: "stale" })).toMatchObject({
    state: "stale",
    tone: "warning",
  })
  expect(presentWorkflow({ selection: identity, availability: "unavailable" })).toMatchObject({
    state: "unavailable",
    tone: "error",
  })
})

test("a new selection is ready instead of inheriting the previous Workflow result", () => {
  const previous = { ...identity, source: "project:previous", name: "previous" }
  const view = presentWorkflow({
    selection: identity,
    availability: "available",
    run: {
      id: "run-previous",
      workflow: previous,
      request_hash: "hash",
      owner: "owner",
      status: "failed",
      stages: [stage("failed", "failed", 0)],
    },
  })

  expect(view).toMatchObject({ state: "ready", name: "release", tone: "info" })
  expect(view.agentProgress).toBeUndefined()
  expect(view.currentWork).toBeUndefined()
})

test("running fan-out is declaration ordered, deduplicated, and bounded", () => {
  const view = presentWorkflow(
    {
      selection: identity,
      availability: "available",
      run: {
        id: "run-1",
        workflow: identity,
        request_hash: "hash",
        owner: "owner",
        status: "running",
        stages: [
          stage("done", "completed", 0, [member("member-done")]),
          stage("alpha", "running", 1, [member("member-a"), member("member-b")]),
          stage("beta", "running", 1, [member("member-c")]),
          stage("gamma", "running", 1, [member("member-b")]),
          stage("later", "pending", 2),
        ],
      },
    },
    [
      sessionMember("member-a", "running", "This deliberately long current task label must truncate"),
      sessionMember("member-b", "spawning"),
      sessionMember("member-c", "running"),
      sessionMember("unrelated-running-member", "running"),
    ],
  )

  expect(view).toMatchObject({
    state: "running",
    tone: "info",
    agentProgress: "3/4 agents",
    stageProgress: "1/5 stages",
    levelProgress: "level 2/3",
    activeStages: "alpha +2",
  })
  expect(view.currentWork?.length).toBeLessThanOrEqual(24)
  expect(view.currentWork?.endsWith("…")).toBe(true)
})

test("current work truncation preserves Unicode scalar boundaries", () => {
  const view = presentWorkflow(
    {
      selection: identity,
      availability: "available",
      run: {
        id: "run-unicode",
        workflow: identity,
        request_hash: "hash",
        owner: "owner",
        status: "running",
        stages: [stage("alpha", "running", 0, [member("member-a")])],
      },
    },
    [sessionMember("member-a", "running", "🔧".repeat(30))],
  )

  expect(Array.from(view.currentWork ?? "")).toHaveLength(24)
  expect(view.currentWork?.endsWith("…")).toBe(true)
  expect(view.currentWork).not.toContain("�")
})

test("failed Workflow identifies its failed Stage and ignores unrelated run-tree members", () => {
  const view = presentWorkflow({
    selection: identity,
    availability: "available",
    unrelated_members: [member("unrelated-running-member")],
    run: {
      id: "run-1",
      workflow: identity,
      request_hash: "hash",
      owner: "owner",
      status: "failed",
      stages: [
        stage("done", "completed", 0, [member("member-done")]),
        stage("compile", "failed", 1, [member("member-failed")], "Compile release contract"),
        stage("later", "skipped", 2),
      ],
    },
  })

  expect(view).toMatchObject({
    state: "failed",
    tone: "error",
    agentProgress: "0/2 agents",
    stageProgress: "1/3 stages",
    levelProgress: "level 2/3",
    currentWork: "Compile release contract",
  })
})

test("optional Workflow route fields do not change compact presentation", () => {
  const base = {
    selection: identity,
    availability: "available",
    run: {
      id: "run-routed",
      workflow: identity,
      request_hash: "hash",
      owner: "owner",
      status: "running",
      stages: [stage("execute", "running", 0, [member("member-routed")])],
    },
  }
  const routed = {
    ...base,
    run: {
      ...base.run,
      stages: [
        {
          ...base.run.stages[0],
          worker_model: {
            id: "fake/primary",
            reasoning: "high",
            fallback: [{ id: "fake/fallback", reasoning: "medium" }],
          },
          selected_worker_model: { index: 0, id: "fake/primary", reasoning: "high" },
          verifier_model: { id: "fake/primary", reasoning: "low", fallback: [] },
          selected_verifier_model: { index: 0, id: "fake/primary", reasoning: "low" },
          route_outcomes: [
            {
              session: "hysec_routed",
              run: "workflow-run-routed",
              stage: "execute",
              member: "member-routed",
              role: "worker",
              iteration: 0,
              step: 0,
              candidate_index: 0,
              model: "fake/primary",
              reasoning: "high",
              failure_class: "none",
            },
          ],
        },
      ],
    },
  }
  const activity = [sessionMember("member-routed", "running", "Execute routed stage")]

  expect(parseWorkflowProjection(routed)).toEqual(parseWorkflowProjection(base))
  expect(presentWorkflow(routed, activity)).toEqual(presentWorkflow(base, activity))
})

test("Workflow boundary rejects malformed synchronized state", () => {
  expect(() => parseWorkflowProjection({ selection: { ...identity, revision: 42 } })).toThrow(
    "workflow.selection.revision",
  )
  expect(presentWorkflow({ selection: { ...identity, revision: 42 } })).toMatchObject({
    state: "invalid",
    tone: "error",
  })
})
