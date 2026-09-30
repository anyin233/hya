import { expect, test } from "bun:test"
import { effortRows, modelRows, relativeTime, sessionRows } from "../src/state/catalog"
import type { ModelSummary, SessionInfo } from "../src/client"

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

const effortModel: ModelSummary = { id: "openai/gpt-6-astra", providerId: "openai", modelId: "gpt-6-astra", reasoning: true, reasoningVariants: ["minimal", "low", "medium", "high"] }


test("the effort picker lists default first, then none, then the advertised variants", () => {
  const rows = effortRows(effortModel, undefined, "default")
  expect(rows.map((row) => ({ id: row.id, tag: row.tag }))).toEqual([
    { id: "default", tag: "default" },
    { id: "none", tag: "off" },
    { id: "minimal", tag: "thinking" },
    { id: "low", tag: "thinking" },
    { id: "medium", tag: "thinking" },
    { id: "high", tag: "thinking" },
  ])
  // No explicit choice: `default` is current and says what it resolves to.
  expect(rows.find((row) => row.current)?.id).toBe("default")
  expect(rows[0]?.detail).toBe("no explicit effort")
  expect(effortRows({ ...effortModel, reasoningDefault: "low" }, undefined, "low")[0]?.detail).toBe("effective low")
  // With an explicit choice that row is current instead.
  const explicit = effortRows({ ...effortModel, reasoningDefault: "low" }, "none", "none")
  expect(explicit.find((row) => row.current)?.id).toBe("none")
  expect(explicit[0]?.detail).toBe("no explicit effort")
  // A model without reasoning support still offers the explicit off switch.
  expect(effortRows({ id: "m/plain", providerId: "m", modelId: "plain", reasoning: false }, undefined, "default").map((row) => row.id)).toEqual(["default", "none"])
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
  { id: "hysec_1", agent: "hya-main", workdir: "/w", title: "Refactor auth", timeUpdated: "2026-09-25T11:00:00Z" },
  { id: "hysec_2", agent: "review", workdir: "/w", parent: "hysec_1", busy: true, timeUpdated: "2026-09-25T11:55:00Z" },
]

test("sessionRows puts a New session row first, then the numbered tree with subagents tagged and busy noted", () => {
  const now = Date.parse("2026-09-25T12:00:00Z")
  const rows = sessionRows(sessions, "hysec_1", now)
  expect(rows[0]).toMatchObject({ id: "__new__", label: "New session" })
  expect(rows[1]).toMatchObject({ id: "hysec_1", label: "1. Refactor auth", current: true })
  // A subagent carries its parent's number (`/open 1.1`).
  expect(rows[2]).toMatchObject({ id: "hysec_2", label: "↳ 1.1 hysec_2" })
  expect(rows[2]?.tag).toBe("subagent")
  expect(rows[2]?.detail).toContain("running")
  expect(rows[1]?.detail).toContain("1h")
})

test("sessionRows numbers only the active Project's sessions, as the sidebar and /open do", () => {
  const scoped: SessionInfo[] = [
    { id: "hysec_o", agent: "hya-main", workdir: "/o", projectId: "prj_o", title: "Elsewhere" },
    { id: "hysec_w", agent: "hya-main", workdir: "/w", projectId: "prj_w", title: "Here" },
  ]
  expect(sessionRows(scoped, undefined, 0, { activeProjectId: "prj_w" }).slice(1).map((row) => row.label)).toEqual(["1. Here"])
  // Rename edits, and a delete confirmation names, the title alone (state/picker.ts `name`).
  expect(sessionRows(scoped, undefined, 0, { activeProjectId: "prj_w" })[1]?.name).toBe("Here")
  // F3 lists every Project; a session outside the active one has no number to type.
  expect(sessionRows(scoped, undefined, 0, { activeProjectId: "prj_w", allProjects: true }).slice(1).map((row) => row.label)).toEqual(["Elsewhere", "1. Here"])
  // The archived view lists sessions the sidebar does not: they have no number, and the others keep theirs.
  const archived: SessionInfo = { id: "hysec_a", agent: "hya-main", workdir: "/w", projectId: "prj_w", title: "Old", archived: true }
  expect(sessionRows([archived, ...scoped], undefined, 0, { activeProjectId: "prj_w", numbered: scoped }).slice(1).map((row) => row.label)).toEqual(["Old", "1. Here"])
})
