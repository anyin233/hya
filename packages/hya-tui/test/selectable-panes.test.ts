import { expect, test } from "bun:test"
import { createAppStore } from "../src/state/store"
import { focusedPane, keyboardOwner } from "../src/state/focus"
import { defaultPaneLayout, movePaneFocus, normalizePaneFocus, paneDefinitions, paneKinds, paneLeaves, parsePaneLayout, type PaneNode, rotatePaneFocus, selectablePanes, setPaneKind, splitPane } from "../src/state/panes"

test("every pane declares eligibility; viewer and informational panes are passive", () => {
  expect(Object.keys(paneDefinitions).sort()).toEqual([...paneKinds].sort())
  for (const kind of ["conversation", "activity", "todos", "context", "status"] as const) expect(paneDefinitions[kind].selectable).toBe(false)
  expect(selectablePanes(defaultPaneLayout().root).map((pane) => pane.kind)).toEqual(["projects", "composer", "sessions"])
})

test("directional navigation skips passive rectangles and leaves the tree mounted", () => {
  const layout = defaultPaneLayout()
  expect(movePaneFocus(layout, "up")).toBe(layout) // Viewer and activity above the editor are passive.
  expect(movePaneFocus(layout, "right").active).toBe("pane-3") // Skip Context alongside the editor.
  expect(movePaneFocus({ ...layout, active: "pane-3" }, "left").active).toBe("pane-1")
  expect(movePaneFocus(layout, "left").root).toBe(layout.root)
})

test("rotation visits only selectable leaves and wraps in both directions", () => {
  const layout = defaultPaneLayout()
  expect(rotatePaneFocus(layout).active).toBe("pane-3")
  expect(rotatePaneFocus({ ...layout, active: "pane-3" }).active).toBe("pane-2")
  expect(rotatePaneFocus({ ...layout, active: "pane-2" }, -1).active).toBe("pane-3")
})

test("adding or assigning a passive pane cannot take input ownership", () => {
  const layout = defaultPaneLayout()
  const passive = splitPane(layout, "vertical", "status")
  expect(passive.active).toBe(layout.active)
  expect(paneLeaves(passive.root).find((pane) => pane.kind === "status")?.kind).toBe("status")
  const jobs = splitPane(layout, "vertical", "jobs")
  const status = setPaneKind(jobs, "status")
  expect(status.active).toBe("pane-1")
  expect(normalizePaneFocus({ ...layout, active: "pane-6" }).active).toBe("pane-1")
})

test("legacy layout migration preserves old editor id, split weights, and eligible focus", () => {
  const legacy = { version: 2, active: "pane-2", root: { type: "split", axis: "vertical", weight: 0.6,
    first: { type: "pane", id: "pane-1", kind: "conversation" }, second: { type: "pane", id: "pane-2", kind: "status" } } }
  const migrated = parsePaneLayout(legacy)!
  expect(migrated.version).toBe(3)
  expect(migrated.active).toBe("pane-1")
  expect(paneLeaves(migrated.root).map((pane) => [pane.id, pane.kind])).toEqual([["pane-3", "conversation"], ["pane-4", "activity"], ["pane-1", "composer"], ["pane-2", "status"]])
  expect(migrated.root.type === "split" && migrated.root.weight).toBe(0.6)
  expect(parsePaneLayout(JSON.parse(JSON.stringify(migrated)))).toEqual(migrated)
})

test("v3 requires one viewer and editor; persisted passive focus repairs to editor", () => {
  const layout = defaultPaneLayout()
  expect(parsePaneLayout({ ...layout, active: "pane-4" })?.active).toBe("pane-1")
  const invalid = JSON.parse(JSON.stringify(layout))
  const edit = (node: PaneNode): void => {
    if (node.type === "pane" && node.kind === "activity") node.kind = "composer"
    else if (node.type === "split") { edit(node.first); edit(node.second) }
  }
  edit(invalid.root)
  expect(parsePaneLayout(invalid)).toBeUndefined()
})

test("there is one focus source, and overlays temporarily own the highlight", () => {
  const store = createAppStore()
  const layout = store.state.paneLayout
  store.setPaneLayout({ ...layout, active: "pane-2" })
  expect(store.state.projectsSidebarFocus).toBe(true)
  expect(focusedPane(store.state)?.id).toBe("pane-2")
  expect(keyboardOwner(store.state, true)).toBe("commands")
  store.setPaneLayout({ ...layout, active: "pane-6" })
  expect(focusedPane(store.state)?.kind).toBe("composer")
  expect(store.state.projectsSidebarFocus).toBe(false)
})
