import { expect, test } from "bun:test"
import { agentRows, modelRows, relativeTime, sessionRows } from "../src/state/catalog"
import type { AgentSummary, ModelSummary, SessionInfo } from "../src/client"

const models: ModelSummary[] = [
  { id: "acme/fast", providerId: "acme", modelId: "fast", displayName: "Fast", contextLimit: "128000" },
  { id: "acme/slow", providerId: "acme", modelId: "slow" },
  { id: "other/x", providerId: "other", modelId: "x" },
]

test("modelRows tags each row with its provider and marks the current model", () => {
  const rows = modelRows(models, "acme/slow")
  expect(rows.map((row) => ({ id: row.id, label: row.label, tag: row.tag, current: row.current }))).toEqual([
    { id: "acme/fast", label: "Fast", tag: "acme", current: false },
    { id: "acme/slow", label: "slow", tag: "acme", current: true },
    { id: "other/x", label: "x", tag: "other", current: false },
  ])
  expect(rows[0]?.detail).toContain("128k")
})

const agents: AgentSummary[] = [
  { name: "build", description: "General-purpose coding agent", model: { providerId: "acme", modelId: "fast" } },
  { name: "review", description: "Read-only review agent", hidden: false },
  { name: "internal", description: "Not for pickers", hidden: true },
]

test("agentRows drops hidden agents and marks the current one", () => {
  const rows = agentRows(agents, "review")
  expect(rows.map((row) => row.id)).toEqual(["build", "review"])
  expect(rows.find((row) => row.id === "build")?.tag).toBe("acme/fast")
  expect(rows.find((row) => row.id === "review")?.current).toBe(true)
})

test("relativeTime formats seconds/minutes/hours/days, and is empty for unset or bad input", () => {
  const now = Date.parse("2026-09-25T12:00:00Z")
  expect(relativeTime("2026-09-25T11:59:30Z", now)).toBe("30s")
  expect(relativeTime("2026-09-25T11:55:00Z", now)).toBe("5m")
  expect(relativeTime("2026-09-25T09:00:00Z", now)).toBe("3h")
  expect(relativeTime("2026-09-20T12:00:00Z", now)).toBe("5d")
  expect(relativeTime(undefined, now)).toBe("")
  expect(relativeTime("not-a-date", now)).toBe("")
})

const sessions: SessionInfo[] = [
  { id: "hysec_1", agent: "build", workdir: "/w", title: "Refactor auth", timeUpdated: "2026-09-25T11:00:00Z" },
  { id: "hysec_2", agent: "review", workdir: "/w", parent: "hysec_1", busy: true, timeUpdated: "2026-09-25T11:55:00Z" },
]

test("sessionRows puts a New session row first, then the tree with subagents tagged and busy noted", () => {
  const now = Date.parse("2026-09-25T12:00:00Z")
  const rows = sessionRows(sessions, "hysec_1", now)
  expect(rows[0]).toMatchObject({ id: "__new__", label: "New session" })
  expect(rows[1]).toMatchObject({ id: "hysec_1", label: "Refactor auth", current: true })
  expect(rows[2]?.id).toBe("hysec_2")
  expect(rows[2]?.tag).toBe("subagent")
  expect(rows[2]?.detail).toContain("running")
  expect(rows[1]?.detail).toContain("1h")
})
