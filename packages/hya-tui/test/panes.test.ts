import { expect, test } from "bun:test"
import { sidebarMinColumns } from "../src/state/layout"
import {
  boundaryWeight, closePane, defaultPaneLayout, movePaneFocus, parsePaneLayout, paneLeaves,
  renderedWeight, resizePane, setPaneKind, setSplitWeight, splitPane, visiblePaneRoot, type PaneLayout, type PaneSplit,
} from "../src/state/panes"

const singlePaneLayout = (): PaneLayout => ({ version: 2, active: "pane-1", root: { type: "pane", id: "pane-1", kind: "conversation" } })

test("nested splits keep one conversation and focus the nearest pane in each direction", () => {
  const leftRight = splitPane(singlePaneLayout(), "vertical", "jobs")
  expect(paneLeaves(leftRight.root).map((pane) => pane.kind)).toEqual(["conversation", "jobs"])
  const nested = splitPane(leftRight, "horizontal", "todos")
  expect(paneLeaves(nested.root).map((pane) => pane.kind)).toEqual(["conversation", "jobs", "todos"])
  expect(movePaneFocus(nested, "left").active).toBe("pane-1")
  expect(movePaneFocus(nested, "up").active).toBe("pane-2")
  expect(movePaneFocus({ ...nested, active: "pane-1" }, "right").active).toBe("pane-2")
})

test("assignment and close preserve the conversation; invalid saved layouts are refused", () => {
  const split = splitPane(singlePaneLayout(), "vertical", "jobs")
  const models = setPaneKind(split, "models")
  expect(paneLeaves(models.root).map((pane) => pane.kind)).toEqual(["conversation", "models"])
  const moved = setPaneKind(models, "conversation")
  expect(paneLeaves(moved.root).map((pane) => pane.kind)).toEqual(["models", "conversation"])
  expect(paneLeaves(closePane(models).root).map((pane) => pane.kind)).toEqual(["conversation"])
  expect(() => closePane(moved)).toThrow("conversation")
  expect(parsePaneLayout(JSON.parse(JSON.stringify(models)))).toEqual(models)
  expect(parsePaneLayout({ ...models, active: "missing" })).toBeUndefined()
  expect(parsePaneLayout({ ...models, root: { ...models.root, weight: 0 } })).toBeUndefined()
})

test("resize changes only the focused split and closes a nested auxiliary pane", () => {
  const nested = splitPane(splitPane(singlePaneLayout(), "vertical", "jobs"), "horizontal", "todos")
  const resized = resizePane(nested, 0.1)
  expect(resized.root.type).toBe("split")
  if (resized.root.type !== "split" || resized.root.second.type !== "split") return
  expect(resized.root.weight).toBe(0.5)
  expect(resized.root.second.weight).toBeCloseTo(0.4)
  const closed = closePane(resized)
  expect(paneLeaves(closed.root).map((pane) => pane.kind)).toEqual(["conversation", "jobs"])
  expect(parsePaneLayout(closed)).toEqual(closed)
})

test("default layout uses half-width project and context sidebars", () => {
  const root = defaultPaneLayout().root
  if (root.type !== "split" || root.second.type !== "split") throw new Error("expected nested vertical splits")
  expect(root.weight).toBe(0.1)
  expect(root.second.weight).toBe(0.88)
})

test("version 2 default layout migrates its legacy sidebar widths", () => {
  const legacy = { ...defaultPaneLayout(), root: { type: "split" as const, axis: "vertical" as const, weight: 0.18, first: { type: "pane" as const, id: "pane-2", kind: "projects" as const }, second: { type: "split" as const, axis: "vertical" as const, weight: 0.74, first: { type: "pane" as const, id: "pane-1", kind: "conversation" as const }, second: { type: "pane" as const, id: "pane-3", kind: "sessions" as const } } } }
  const migrated = parsePaneLayout(legacy)
  expect(migrated?.root.type).toBe("split")
  if (migrated?.root.type !== "split" || migrated.root.second.type !== "split") return
  expect(migrated.root.weight).toBe(0.1)
  expect(migrated.root.second.weight).toBe(0.88)
})

/** The Conversation | Sessions/Todos/Context split of the default layout, as shown below the Projects breakpoint. */
function rightSplit(layout: PaneLayout = defaultPaneLayout()): PaneSplit {
  const root = visiblePaneRoot(layout.root, 150, "auto", "closed")
  if (root.type !== "split") throw new Error("expected the right sidebar split")
  return root
}

test("the right sidebar is drawn at least sidebarMinColumns wide; wider when its weight allows", () => {
  const split = rightSplit()
  // 12% of 150 columns is 18: the drawn weight leaves the sidebar exactly its minimum.
  expect(150 * (1 - renderedWeight(split, 150))).toBeCloseTo(sidebarMinColumns)
  // 12% of 300 columns is 36, above the minimum: the saved weight is drawn as is.
  expect(renderedWeight(split, 300)).toBe(0.88)
  // Only a split whose second side is the right sidebar is held open.
  const jobs = splitPane({ version: 2, active: "pane-1", root: { type: "pane", id: "pane-1", kind: "conversation" } }, "vertical", "jobs").root as PaneSplit
  expect(renderedWeight({ ...jobs, weight: 0.9 }, 150)).toBe(0.9)
})

test("dragging a split boundary to a column sets its weight within the limits", () => {
  const split = rightSplit()
  // Split drawn from column 0, 200 columns wide: the boundary at column 120 is weight 0.6.
  expect(boundaryWeight(split, 0, 200, 120)).toBeCloseTo(0.6)
  expect(boundaryWeight(split, 20, 200, 140)).toBeCloseTo(0.6)
  // Dragged past the sidebar's minimum: it stops at sidebarMinColumns.
  expect(200 * (1 - boundaryWeight(split, 0, 200, 199))).toBeCloseTo(sidebarMinColumns)
  // Dragged to the far left: the conversation keeps 10%.
  expect(boundaryWeight(split, 0, 200, 0)).toBe(0.1)
})

test("setSplitWeight changes the split between two panes in the saved tree and nothing else", () => {
  const layout = defaultPaneLayout()
  const next = setSplitWeight(layout, "pane-1", "pane-3", 0.7)
  if (next.root.type !== "split" || next.root.second.type !== "split") throw new Error("expected nested vertical splits")
  expect(next.root.weight).toBe(0.1)
  expect(next.root.second.weight).toBe(0.7)
  expect(rightSplit(next).weight).toBe(0.7)
  expect(parsePaneLayout(JSON.parse(JSON.stringify(next)))).toEqual(next)
  // No split has pane-3 on its first side and pane-1 on its second: nothing to resize.
  expect(setSplitWeight(layout, "pane-3", "pane-1", 0.7)).toBe(layout)
})
