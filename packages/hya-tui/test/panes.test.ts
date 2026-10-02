import { expect, test } from "bun:test"
import {
  closePane, defaultPaneLayout, movePaneFocus, parsePaneLayout, paneLeaves, paneNodes,
  resizePane, setPaneKind, splitPane, visiblePaneRoot, layoutRects, normalizePaneNode,
  insertPane, movePane, wrapPane, setContainerBoundary, rotatePaneFocus,
  type PaneLayout, type PaneNode, type PaneSplit, type Rect,
} from "../src/state/panes"
const leaf = (id: number, kind: "conversation" | "composer" | "jobs" | "sessions" | "status" = "jobs"): PaneNode => ({ type: "pane", id: `pane-${id}`, kind })
const weight = (node: PaneNode, value = 1) => ({ node, size: { mode: "weight" as const, value } })
const content = (node: PaneNode) => ({ node, size: { mode: "content" as const } })
const group = (id: number, direction: "row" | "column", children: PaneSplit["children"]): PaneSplit => ({ type: "split", id: `group-${id}`, direction, children })
const singlePaneLayout = (): PaneLayout => ({ version: 4, active: "pane-1", root: leaf(1, "composer") })

test("row siblings flatten and retain multiplied ratios", () => {
  const root = group(1, "row", [weight(leaf(1), .2), weight(group(2, "row", [weight(leaf(2), .25), weight(leaf(3), .75)]), .8)])
  const flat = normalizePaneNode(root) as PaneSplit
  expect(flat.children.map((child) => child.size)).toEqual([{ mode: "weight", value: .2 }, { mode: "weight", value: .2 }, { mode: "weight", value: .6000000000000001 }])
  expect(normalizePaneNode(flat)).toBe(flat)
  const bounds = layoutRects(flat, { left: 0, top: 0, right: 100, bottom: 40 })
  expect(bounds.get("pane-1")?.right).toBe(20)
  expect(bounds.get("pane-2")?.right).toBe(40)
})

test("normalization keeps mixed sizing boundaries and flattens content-only docks", () => {
  const dock = group(2, "column", [content(leaf(2)), content(leaf(3))])
  const flat = normalizePaneNode(group(1, "column", [weight(leaf(1)), content(dock)])) as PaneSplit
  expect(flat.children).toHaveLength(3)
  const mixed = group(3, "column", [weight(leaf(4)), content(leaf(5))])
  const retained = normalizePaneNode(group(4, "column", [weight(leaf(6)), weight(mixed)])) as PaneSplit
  expect(retained.children[1]?.node.id).toBe("group-3")
})

test("binary v3 migration preserves weighted geometry, focus and ids", () => {
  const old = { version: 3, active: "pane-3", root: { type: "split", axis: "vertical", weight: .2,
    first: { type: "pane", id: "pane-1", kind: "conversation" }, second: { type: "split", axis: "vertical", weight: .25,
      first: { type: "pane", id: "pane-2", kind: "composer" }, second: { type: "pane", id: "pane-3", kind: "jobs" } } } }
  const layout = parsePaneLayout(old)!
  expect(layout.version).toBe(4)
  expect(layout.active).toBe("pane-3")
  const rects = layoutRects(layout.root, { left: 0, top: 0, right: 100, bottom: 40 })
  expect(rects.get("pane-2")?.left).toBe(20)
  expect(rects.get("pane-3")?.left).toBe(40)
  expect(parsePaneLayout(layout)).toEqual(layout)
})

test("legacy default content dock becomes viewer plus two content leaves", () => {
  const old = { version: 3, active: "pane-1", root: { type: "split", axis: "horizontal", weight: .8,
    first: { type: "pane", id: "pane-2", kind: "conversation" }, second: { type: "split", axis: "horizontal", weight: .2,
      first: { type: "pane", id: "pane-3", kind: "activity" }, second: { type: "pane", id: "pane-1", kind: "composer" } } } }
  const migrated = parsePaneLayout(old)!
  expect(migrated.root.type === "split" && migrated.root.children.map((child) => child.size.mode)).toEqual(["weight", "content", "content"])
  const custom = { ...old, root: { ...old.root, weight: .7 } }
  expect((parsePaneLayout(custom)!.root as PaneSplit).children.every((child) => child.size.mode === "weight")).toBe(true)
})

test("solver honors minimum widths, content rows, zero idle activity and screen bounds", () => {
  const layout = defaultPaneLayout()
  const minimum = (node: PaneNode, direction: "row" | "column"): number => {
    if (node.type === "split") return direction === "column" ? 3 : node.id === "group-3" ? 28 : 12
    return direction === "row" ? 8 : node.kind === "activity" ? 0 : 3
  }
  const rects = layoutRects(layout.root, { left: 0, top: 0, right: 150, bottom: 40 }, minimum)
  expect(rects.get("group-3")!.right - rects.get("group-3")!.left).toBeGreaterThanOrEqual(28)
  expect(rects.get("pane-7")!.bottom - rects.get("pane-7")!.top).toBe(0)
  expect(rects.get("pane-1")!.bottom - rects.get("pane-1")!.top).toBe(3)
  expect(rects.get("pane-6")!.bottom).toBe(rects.get("pane-1")!.top)
  const tiny = layoutRects(layout.root, { left: 0, top: 0, right: 4, bottom: 2 }, minimum)
  for (const rect of tiny.values()) { expect(rect.left).toBeGreaterThanOrEqual(0); expect(rect.right).toBeLessThanOrEqual(4); expect(rect.bottom).toBeLessThanOrEqual(2) }
})

test("tree operations insert, move, wrap root and prune closed auxiliary branches", () => {
  const start = defaultPaneLayout()
  const inserted = insertPane(start, "root", 1, "jobs")
  expect(paneLeaves(inserted.root).map((pane) => pane.kind).slice(0, 2)).toEqual(["projects", "jobs"])
  const moved = movePane(inserted, "pane-8", "group-3", 1)
  expect((paneNodes(moved.root).find((node) => node.id === "group-3") as PaneSplit).children[1]?.node.id).toBe("pane-8")
  expect(moved.active).toBe("pane-8")
  const wrapped = wrapPane(moved, "root", "column", "models", false)
  expect(wrapped.root.type === "split" && wrapped.root.direction).toBe("column")
  expect(paneLeaves(wrapped.root).at(-1)?.kind).toBe("models")
  expect(parsePaneLayout(wrapped)).toEqual(wrapped)
  const closed = closePane(wrapped, "models")
  expect(closed.root.id).toBe(moved.root.id)
  expect(() => movePane(start, "root", "group-2", 0)).toThrow("descendant")
  expect(() => insertPane(start, "root", 100, "jobs")).toThrow("index")
  expect(() => wrapPane(start, "root", "row", "composer")).toThrow("assign")
  expect(() => closePane(start, "group-2")).toThrow("Cannot close")
})

test("moving within a parent uses post-removal indexes and preserves singleton leaves", () => {
  const layout = defaultPaneLayout()
  const moved = movePane(layout, "composer", "group-2", 0)
  const center = paneNodes(moved.root).find((node) => node.id === "group-2") as PaneSplit
  expect(center.children[0]?.node.id).toBe("pane-1")
  expect(paneLeaves(moved.root).filter((pane) => pane.kind === "composer")).toHaveLength(1)
  expect(parsePaneLayout(moved)).toEqual(moved)
  const intoRow = movePane(layout, "composer", "root", 1)
  expect(parsePaneLayout(intoRow)).toEqual(intoRow)
  expect(() => movePane(layout, "group-2", "group-2", 0)).toThrow("itself")
})

test("close removes an entire auxiliary container without leaving empty parents", () => {
  const layout = wrapPane(defaultPaneLayout(), "todos", "row", "jobs")
  const container = paneNodes(layout.root).find((node) => node.type === "split" && paneLeaves(node).every((pane) => ["todos", "jobs"].includes(pane.kind)))!
  const closed = closePane(layout, container.id)
  expect(paneLeaves(closed.root).some((pane) => ["todos", "jobs"].includes(pane.kind))).toBe(false)
  expect(parsePaneLayout(closed)).toEqual(closed)
})

test("boundary dragging and selected resize affect sibling weights and persist", () => {
  const layout = defaultPaneLayout()
  const changed = setContainerBoundary(layout, "group-1", 0, .3)
  expect(changed.root.type === "split" && changed.root.children[0]?.size).toEqual({ mode: "weight", value: .2676 })
  expect(changed.root.type === "split" && changed.root.children[2]).toEqual(layout.root.type === "split" && layout.root.children[2])
  expect(parsePaneLayout(changed)).toEqual(changed)
  const resized = resizePane({ ...layout, active: "pane-2" }, .05)
  expect((resized.root as PaneSplit).children[0]?.size).toEqual({ mode: "weight", value: .15000000000000002 })
})

test("rendered rectangle navigation reaches all eligible panes and uses vertical overlap", () => {
  const layout = splitPane(splitPane(defaultPaneLayout(), "vertical", "jobs"), "horizontal", "models")
  const rects = new Map<string, Rect>([
    ["pane-1", { left: 0, top: 20, right: 20, bottom: 25 }],
    ["pane-2", { left: 40, top: 0, right: 60, bottom: 10 }],
    ["pane-3", { left: 0, top: 0, right: 20, bottom: 10 }],
    ["pane-8", { left: 30, top: 10, right: 50, bottom: 20 }],
    ["pane-9", { left: 0, top: 10, right: 20, bottom: 20 }],
    ["pane-4", { left: 0, top: 0, right: 1, bottom: 1 }], // passive
  ])
  let current = { ...layout, active: "pane-3" }; const seen: string[] = []
  for (let i = 0; i < 5; i++) { seen.push(current.active); current = movePaneFocus(current, "right", rects) }
  expect(seen).toEqual(["pane-3", "pane-2", "pane-9", "pane-8", "pane-1"])
  expect(current.active).toBe("pane-3")
  expect(movePaneFocus(current, "left", rects).active).toBe("pane-1")
  expect(movePaneFocus({ ...layout, active: "pane-1" }, "up", rects).active).toBe("pane-9")
  expect(movePaneFocus({ ...layout, active: "pane-3" }, "up", rects).active).toBe("pane-3")
  const hidden = new Map(rects); hidden.set("pane-9", { left: 0, top: 0, right: 0, bottom: 0 })
  expect(rotatePaneFocus({ ...layout, active: "pane-2" }, 1, hidden).active).toBe("pane-8")
})

test("v4 rejects malformed containers, duplicate ids, unsafe weights and missing singletons", () => {
  const layout = defaultPaneLayout()
  for (const value of [0, -1, NaN, Infinity]) {
    const root = JSON.parse(JSON.stringify(layout.root))
    root.children[0].size.value = value
    expect(parsePaneLayout({ ...layout, root })).toBeUndefined()
  }
  for (const root of [group(1, "row", []), group(1, "row", [weight(leaf(1))]), group(1, "row", [weight(leaf(1, "conversation")), weight(leaf(1, "composer"))])]) expect(parsePaneLayout({ ...layout, root })).toBeUndefined()
  expect(parsePaneLayout({ ...layout, active: "group-1" })).toBeUndefined()
  expect(parsePaneLayout({ ...layout, active: "pane-6" })?.active).toBe("pane-1")
})

test("named close removes passive panes without changing input focus", () => {
  const layout = defaultPaneLayout()
  const closed = closePane(layout, "todos")
  expect(paneLeaves(closed.root).some((pane) => pane.kind === "todos")).toBe(false)
  expect(closed.active).toBe(layout.active)
  expect(parsePaneLayout(closed)).toEqual(closed)
  expect(paneLeaves(closePane(closed, "pane-5").root).some((pane) => pane.kind === "context")).toBe(false)
  expect(() => closePane(layout, "missing")).toThrow("Unknown pane")
  for (const target of ["conversation", "composer", "pane-6", "pane-1"]) expect(() => closePane(layout, target)).toThrow("Cannot close")
})

test("duplicate pane names require an id; unnamed close still uses selection", () => {
  const layout = splitPane(defaultPaneLayout(), "vertical", "todos")
  expect(() => closePane(layout, "todos")).toThrow("pane-8")
  const closed = closePane(layout, "pane-8")
  expect(paneLeaves(closed.root).filter((pane) => pane.kind === "todos")).toHaveLength(1)
  const jobs = splitPane(closed, "vertical", "jobs")
  expect(closePane(jobs).active).toBe("pane-1")
  expect(paneLeaves(closePane(jobs).root).some((pane) => pane.id === jobs.active)).toBe(false)
})


test("splits can prepend the new leaf and retain passive focus and persistence", () => {
  for (const axis of ["horizontal", "vertical"] as const) {
    const layout = splitPane(defaultPaneLayout(), axis, "status", true)
    const leaves = paneLeaves(layout.root)
    expect(leaves.findIndex((pane) => pane.id === "pane-8")).toBeLessThan(leaves.findIndex((pane) => pane.id === "pane-1"))
    expect(layout.active).toBe("pane-1")
    expect(parsePaneLayout(layout)).toEqual(layout)
  }
})


test("dragging a visible pair skips hidden/content siblings without changing them", () => {
  const layout = defaultPaneLayout()
  const before = (layout.root as PaneSplit).children[1]
  const next = setContainerBoundary(layout, "group-1", 0, .4, 2)
  expect((next.root as PaneSplit).children[1]).toBe(before)
  expect(() => setContainerBoundary(layout, "group-1", 1, .4, 1)).toThrow("boundary")
})

test("all-content containers do not stretch their last child into unused space", () => {
  const root = group(1, "column", [content(leaf(1)), content(leaf(2))])
  const rects = layoutRects(root, { left: 0, top: 0, right: 80, bottom: 40 }, () => 3)
  expect(rects.get("pane-1")?.bottom).toBe(3)
  expect(rects.get("pane-2")?.bottom).toBe(6)
})
