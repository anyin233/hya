import { expect, test } from "bun:test"
import { createAppStore } from "../src/state/store"
import { subagentRows } from "../src/state/subagents"
import { defaultPaneLayout, paneLeaves, parsePaneLayout, pinSubagentViewer, splitPane } from "../src/state/panes"

function family() {
  const store = createAppStore()
  store.openSession({ id: "parent", agent: "main", workdir: "/work" })
  store.setSessions([{ id: "parent", agent: "main", workdir: "/work" }, { id: "a", agent: "task", workdir: "/work", parent: "parent", title: "First", busy: true },
    { id: "b", agent: "task", workdir: "/work", parent: "a", title: "Nested" }, { id: "other", agent: "main", workdir: "/work" }])
  store.setMembers([{ member: "m", child: "a", status: "MEMBER_STATUS_DONE", description: "Survey" }])
  return store
}
test("selector keeps completed members, nests descendants and excludes unrelated sessions", () => {
  const store = family()
  expect(subagentRows(store.state).map(({ id, depth, status, label }) => ({ id, depth, status, label }))).toEqual([
    { id: "a", depth: 0, status: "done", label: "Survey" }, { id: "b", depth: 1, status: "idle", label: "Nested" },
  ])
  store.selectSubagent("b")
  expect(store.state.selected?.id).toBe("parent")
  store.openSession({ id: "other", agent: "main", workdir: "/work" })
  expect(store.state.subagentSelection).toBeUndefined()
})
test("viewer pins round-trip through layout reload; passive viewers never acquire focus", () => {
  let layout = splitPane(defaultPaneLayout(), "horizontal", "subagent-viewer")
  const viewer = paneLeaves(layout.root).find((pane) => pane.kind === "subagent-viewer")!
  layout = pinSubagentViewer(layout, viewer.id, "a")
  const parsed = parsePaneLayout(JSON.parse(JSON.stringify(layout)))!
  expect(paneLeaves(parsed.root).find((pane) => pane.id === viewer.id)?.session).toBe("a")
  expect(parsed.active).not.toBe(viewer.id)
  expect(paneLeaves(pinSubagentViewer(parsed, viewer.id).root).find((pane) => pane.id === viewer.id)?.session).toBeUndefined()
  expect(() => pinSubagentViewer(layout, "pane-1", "a")).toThrow("Not a subagent viewer")
  const invalid = JSON.parse(JSON.stringify(layout))
  const composer = paneLeaves(invalid.root).find((pane) => pane.kind === "composer")!
  composer.session = "a"
  expect(parsePaneLayout(invalid)).toBeUndefined()
})

test("archived direct and nested children are hidden, remain tracked, and reappear on revival", () => {
  const store = family()
  const member = { member: "m", child: "a", description: "Survey", status: "MEMBER_STATUS_DONE", handle: "main/a", archived: true, archiveReason: "reported" }
  store.setMembers([member])
  store.setChild("a", { busy: false, members: [{ member: "nested", child: "b", status: "MEMBER_STATUS_FAILED", archived: true, archiveReason: "archived_by_parent", handle: "main/a/b" }] })
  expect(subagentRows(store.state, { includeArchived: true })[0]).toMatchObject({ status: "done", archived: true, handle: "main/a", archiveReason: "reported" })
  expect(subagentRows(store.state, { includeArchived: true })[1]).toMatchObject({ status: "failed", archived: true, handle: "main/a/b", archiveReason: "archived_by_parent" })
  expect(subagentRows(store.state)).toEqual([])
  expect(store.completionContext().subagents).toEqual([])
  store.selectSubagent("a")
  expect(store.state.subagentSelection).toBe("a")
  store.applyEvent({ seq: "1", session: "parent", memberLifecycleChanged: { child: "b", handle: "main/a/b" } })
  expect(subagentRows(store.state, { includeArchived: true })[1]).toMatchObject({ status: "failed", archived: false, archiveReason: "" })
  expect(subagentRows(store.state).map((row) => row.id)).toEqual(["b"])
  store.applyEvent({ seq: "2", session: "parent", memberLifecycleChanged: { child: "a", handle: "main/a" } })
  expect(subagentRows(store.state, { includeArchived: true })[0]).toMatchObject({ status: "done", archived: false, archiveReason: "" })
  store.applyEvent({ seq: "2", session: "parent", memberLifecycleChanged: { child: "a", archived: true, archiveReason: "shutdown" } })
  expect(subagentRows(store.state, { includeArchived: true })[0]?.archived).toBe(false)
  expect(subagentRows(store.state).map((row) => row.id)).toEqual(["a", "b"])
  store.openSession({ id: "parent", agent: "main", workdir: "/work", members: [member] })
  expect(subagentRows(store.state).some((row) => row.id === "a")).toBe(false)
  expect(subagentRows(store.state, { includeArchived: true })[0]).toMatchObject({ status: "done", archived: true, archiveReason: "reported" })
})


test("archive visibility shares selection and completion, preserves tracking, and resets for another conversation", () => {
  const store = family()
  store.setMembers([{ member: "m", child: "a", status: "MEMBER_STATUS_DONE", archived: true }])
  store.selectSubagent("b")
  store.setSubagentsShowArchived(true)
  expect(subagentRows(store.state).map((row) => row.id)).toEqual(["a", "b"])
  expect(store.state.subagentSelection).toBe("b")
  expect(store.completionContext().subagents).toEqual(["a", "b"])
  store.selectSubagent("a")
  store.setSubagentsShowArchived(false)
  expect(store.state.subagentSelection).toBe("b")
  expect(store.completionContext().subagents).toEqual(["b"])
  expect(subagentRows(store.state, { includeArchived: true })).toHaveLength(2)
  store.setSubagentsShowArchived(true)
  expect(subagentRows(store.state, { includeArchived: false }).map((row) => row.id)).toEqual(["b"])
  store.openSession({ id: "other", agent: "main", workdir: "/work" })
  expect(store.state.subagentsShowArchived).toBe(false)
})
