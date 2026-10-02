import { expect, test } from "bun:test"
import { createAppStore } from "../src/state/store"
import { focusedPane, keyboardOwner } from "../src/state/focus"
import { defaultPaneLayout, movePaneFocus, normalizePaneFocus, paneDefinitions, paneKinds, paneLeaves, parsePaneLayout, type PaneNode, paneNodes, resizePane, rotatePaneFocus, selectablePanes, setPaneKind, splitPane } from "../src/state/panes"

test("every pane declares eligibility; viewer and informational panes are passive", () => {
  expect(Object.keys(paneDefinitions).sort()).toEqual([...paneKinds].sort())
  for (const kind of ["conversation", "activity", "todos", "context", "status"] as const) expect(paneDefinitions[kind].selectable).toBe(false)
  expect(selectablePanes(defaultPaneLayout().root).map((pane) => pane.kind)).toEqual(["projects", "composer", "sessions"])
})

test("rotation uses top-to-bottom visual order and wraps across every selectable leaf", () => {
  const layout = defaultPaneLayout()
  expect(rotatePaneFocus(layout).active).toBe("pane-2")
  expect(rotatePaneFocus({ ...layout, active: "pane-2" }).active).toBe("pane-3")
  expect(rotatePaneFocus({ ...layout, active: "pane-3" }).active).toBe("pane-1")
  expect(movePaneFocus(layout, "left").active).toBe("pane-3")
  expect(movePaneFocus(layout, "right").active).toBe("pane-2")
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
  expect(migrated.version).toBe(4)
  expect(migrated.active).toBe("pane-1")
  expect(paneLeaves(migrated.root).map((pane) => [pane.id, pane.kind])).toEqual([["pane-3", "conversation"], ["pane-4", "activity"], ["pane-1", "composer"], ["pane-2", "status"]])
  expect(migrated.root.type === "split" && migrated.root.children[0]?.size).toEqual({ mode: "weight", value: 0.6 })
  expect(parsePaneLayout(JSON.parse(JSON.stringify(migrated)))).toEqual(migrated)
})

test("v3 requires one viewer and editor; persisted passive focus repairs to editor", () => {
  const layout = defaultPaneLayout()
  expect(parsePaneLayout({ ...layout, active: "pane-4" })?.active).toBe("pane-1")
  const invalid = JSON.parse(JSON.stringify(layout))
  const edit = (node: PaneNode): void => {
    if (node.type === "pane" && node.kind === "activity") node.kind = "composer"
    else if (node.type === "split") for (const child of node.children) edit(child.node)
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


test("resize converts the selected content dock to weights and round-trips", () => {
  const resized = resizePane(defaultPaneLayout(), .1)
  const composerParent = paneNodes(resized.root).find((node) => node.type === "split" && node.children.some((child) => child.node.id === "pane-1"))
  expect(composerParent?.type === "split" && composerParent.children.every((child) => child.size.mode === "weight")).toBe(true)
  expect(parsePaneLayout(JSON.parse(JSON.stringify(resized)))).toEqual(resized)
})

test("content sizing is allowed only in column containers", () => {
  const layout = defaultPaneLayout()
  if (layout.root.type !== "split") throw new Error("expected row")
  const children = [...layout.root.children]
  children[0] = { ...children[0]!, size: { mode: "content" } }
  expect(parsePaneLayout({ ...layout, root: { ...layout.root, children } })).toBeUndefined()
})
