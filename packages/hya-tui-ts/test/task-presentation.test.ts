import { expect, test } from "bun:test"
import {
  flattenMemberNodes,
  launchedMembersFromTree,
  resolveTaskMembers,
  resolveTaskSessionId,
} from "../src/upstream/routes/session/task-presentation"
import type { RunTreeNode } from "../src/upstream/routes/session/subagent-workspace"

const tree: RunTreeNode = {
  session: "ses_main",
  agent: "build",
  children: [
    {
      session: "ses_explore",
      member: {
        member: "m1",
        child: "ses_explore",
        subagent_type: "explore",
        description: "Inspect routing",
        depth: 1,
        status: "running",
        summary: "",
      },
      children: [],
    },
    {
      session: "ses_plan",
      member: {
        member: "m2",
        child: "ses_plan",
        subagent_type: "plan",
        description: "Draft plan",
        depth: 1,
        status: "done",
        summary: "ok",
      },
      children: [],
    },
  ],
}

test("resolveTaskMembers expands multi-member metadata", () => {
  const members = resolveTaskMembers({
    input: {},
    metadata: {
      members: [
        {
          description: "Inspect tools",
          subagent_type: "explore",
          sessionId: "ses_a",
          status: "done",
        },
        {
          description: "Inspect runtime",
          subagent_type: "explore",
          session: "ses_b",
          status: "done",
        },
      ],
    },
  })
  expect(members).toHaveLength(2)
  expect(members[0]).toMatchObject({
    description: "Inspect tools",
    sessionId: "ses_a",
    subagentType: "explore",
  })
  expect(members[1]?.sessionId).toBe("ses_b")
})

test("resolveTaskMembers falls back to single task input", () => {
  const members = resolveTaskMembers({
    input: { description: "Inspect routing", subagent_type: "explore" },
    metadata: { sessionId: "ses_explore", background: true },
  })
  expect(members).toEqual([
    {
      sessionId: "ses_explore",
      subagentType: "explore",
      description: "Inspect routing",
      background: true,
      status: undefined,
    },
  ])
})

test("resolveTaskMembers uses input.members while still running", () => {
  const members = resolveTaskMembers({
    input: {
      members: [
        { description: "One", subagent_type: "explore", prompt: "p1" },
        { description: "Two", subagent_type: "plan", prompt: "p2" },
      ],
    },
    metadata: {},
  })
  expect(members.map((m) => m.description)).toEqual(["One", "Two"])
  expect(members.every((m) => !m.sessionId)).toBe(true)
})

test("resolveTaskSessionId matches run tree by description", () => {
  const member = {
    subagentType: "explore",
    description: "Inspect routing",
    background: false,
  }
  expect(resolveTaskSessionId(member, tree)).toBe("ses_explore")
})

test("flattenMemberNodes and launchedMembersFromTree skip covered sessions", () => {
  expect(flattenMemberNodes(tree)).toHaveLength(2)
  const uncovered = launchedMembersFromTree(tree, new Set(["ses_explore"]))
  expect(uncovered).toHaveLength(1)
  expect(uncovered[0]?.sessionId).toBe("ses_plan")
  expect(uncovered[0]?.description).toBe("Draft plan")
})
