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
