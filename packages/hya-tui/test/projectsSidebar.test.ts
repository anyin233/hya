import { expect, test } from "bun:test"
import type { ProjectInfo } from "../src/client"
import { projectSidebarEntries, projectSidebarRows, projectsSidebarKey } from "../src/state/projectsSidebar"

const key = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; sequence: string }> = {}) =>
  ({ name, ctrl: false, meta: false, shift: false, sequence: "", ...extra })

const projects: ProjectInfo[] = [
  { id: "a", name: "alpha", roots: ["/a"], busy: true, sessionCount: 3 },
  { id: "b", name: "beta", roots: ["/b"], sessionCount: 0 },
]

test("projectSidebarRows carries the busy marker, session count, and the active row", () => {
  const rows = projectSidebarRows(projects, "b")
  expect(rows).toEqual([
    { id: "a", name: "alpha", busy: true, sessionCount: 3, active: false },
    { id: "b", name: "beta", busy: false, sessionCount: 0, active: true },
  ])
})

test("Up/Down move the highlight with wrap-around; Enter switches; Esc blurs", () => {
  const rows = projectSidebarRows(projects, "a")
  expect(projectsSidebarKey(key("down"), rows, "a")).toEqual({ type: "move", id: "b" })
  expect(projectsSidebarKey(key("down"), rows, "b")).toEqual({ type: "move", id: "a" })
  expect(projectsSidebarKey(key("up"), rows, "a")).toEqual({ type: "move", id: "b" })
  expect(projectsSidebarKey(key("return"), rows, "b")).toEqual({ type: "switch", id: "b" })
  expect(projectsSidebarKey(key("escape"), rows, "a")).toEqual({ type: "blur" })
})

test("the rendered list separates Projects with a rule, like the Sessions list", () => {
  expect(projectSidebarEntries(projects, "a")).toEqual([
    { row: { id: "a", name: "alpha", busy: true, sessionCount: 3, active: true } },
    { separator: true },
    { row: { id: "b", name: "beta", busy: false, sessionCount: 0, active: false } },
  ])
  // A single Project (and an empty list) draws no rule.
  expect(projectSidebarEntries([projects[0]!], "a")).toEqual([{ row: { id: "a", name: "alpha", busy: true, sessionCount: 3, active: true } }])
  expect(projectSidebarEntries([], undefined)).toEqual([])
})

test("an empty row list moves nowhere", () => {
  expect(projectsSidebarKey(key("down"), [], undefined)).toEqual({ type: "none" })
})
