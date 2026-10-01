/** Local split tree for the whole workspace. Backend projections are shared by its panes. */
import { projectsSidebarVisible, sidebarMinColumns, sidebarVisible, type SidebarMode } from "./layout"

export const paneKinds = ["conversation", "projects", "jobs", "sessions", "todos", "context", "models", "workflows", "interactions", "status", "api"] as const
export type PaneKind = typeof paneKinds[number]
export type PaneAxis = "horizontal" | "vertical"
export type PaneDirection = "left" | "right" | "up" | "down"

export interface PaneLeaf {
  type: "pane"
  id: string
  kind: PaneKind
}

export interface PaneSplit {
  type: "split"
  axis: PaneAxis
  /** Fraction of available width/height allocated to `first`. */
  weight: number
  first: PaneNode
  second: PaneNode
}

export type PaneNode = PaneLeaf | PaneSplit
export interface PaneLayout {
  version: 2
  root: PaneNode
  active: string
}

export const maxPanes = 16

export function defaultPaneLayout(): PaneLayout {
  const leaf = (id: number, kind: PaneKind): PaneLeaf => ({ type: "pane", id: `pane-${id}`, kind })
  return { version: 2, active: "pane-1", root: {
    type: "split", axis: "vertical", weight: 0.1, first: leaf(2, "projects"), second: {
      type: "split", axis: "vertical", weight: 0.88, first: leaf(1, "conversation"), second: {
        type: "split", axis: "horizontal", weight: 0.62, first: leaf(3, "sessions"), second: {
          type: "split", axis: "horizontal", weight: 0.36, first: leaf(4, "todos"), second: leaf(5, "context"),
        },
      },
    },
  } }
}


/** Filter pane jobs by viewport without depending on the currently focused pane. */
export function visiblePaneRoot(root: PaneNode, columns: number, sidebar: SidebarMode, projectsSidebar: SidebarMode): PaneNode {
  const right = sidebarVisible(sidebar, columns)
  const left = projectsSidebarVisible(projectsSidebar, columns)
  const keep = (node: PaneNode): PaneNode | undefined => {
    if (node.type === "pane") {
      if (node.kind === "projects" && !left) return undefined
      if (["sessions", "todos", "context"].includes(node.kind) && !right) return undefined
      return node
    }
    const first = keep(node.first)
    const second = keep(node.second)
    if (!first) return second
    if (!second) return first
    if (first === node.first && second === node.second) return node
    return { ...node, first, second }
  }
  return keep(root) ?? { type: "pane", id: "pane-1", kind: "conversation" }
}

/** Keep focus on a pane that remains visible after responsive filtering. */
export function visiblePaneLayout(layout: PaneLayout, columns: number, sidebar: SidebarMode, projectsSidebar: SidebarMode): PaneLayout {
  const root = visiblePaneRoot(layout.root, columns, sidebar, projectsSidebar)
  const leaves = paneLeaves(root)
  return { ...layout, root, active: leaves.some((pane) => pane.id === layout.active) ? layout.active : leaves.find((pane) => pane.kind === "conversation")?.id ?? leaves[0]!.id }
}

export function paneLeaves(node: PaneNode): PaneLeaf[] {
  return node.type === "pane" ? [node] : [...paneLeaves(node.first), ...paneLeaves(node.second)]
}

function mapPane(node: PaneNode, id: string, change: (pane: PaneLeaf) => PaneNode): PaneNode {
  if (node.type === "pane") return node.id === id ? change(node) : node
  return { ...node, first: mapPane(node.first, id, change), second: mapPane(node.second, id, change) }
}

/** Split the active rectangle into two equal rectangles; the new pane gets focus. */
export function splitPane(layout: PaneLayout, axis: PaneAxis, kind: PaneKind = "jobs"): PaneLayout {
  const leaves = paneLeaves(layout.root)
  if (leaves.length >= maxPanes) throw new Error(`A layout supports at most ${maxPanes} panes`)
  if (kind === "conversation") throw new Error("Use /layout assign conversation to move the conversation pane")
  const next = Math.max(...leaves.map((leaf) => Number(leaf.id.match(/^pane-(\d+)$/)?.[1] ?? 0))) + 1
  const id = `pane-${next}`
  return {
    ...layout,
    root: mapPane(layout.root, layout.active, (pane) => ({ type: "split", axis, weight: 0.5, first: pane, second: { type: "pane", id, kind } })),
    active: id,
  }
}

/** Assign a job. Assigning conversation swaps it with the sole conversation pane. */
export function setPaneKind(layout: PaneLayout, kind: PaneKind): PaneLayout {
  const leaves = paneLeaves(layout.root)
  const active = leaves.find((leaf) => leaf.id === layout.active)
  if (!active) return layout
  if (active.kind === kind) return layout
  if (active.kind === "conversation") throw new Error("Move conversation to another pane before assigning this pane")
  if (kind !== "conversation") return { ...layout, root: mapPane(layout.root, layout.active, (pane) => ({ ...pane, kind })) }
  const conversation = leaves.find((leaf) => leaf.kind === "conversation")
  if (!conversation) return layout
  const swapped = mapPane(layout.root, conversation.id, (pane) => ({ ...pane, kind: active.kind }))
  return { ...layout, root: mapPane(swapped, active.id, (pane) => ({ ...pane, kind: "conversation" })) }
}

/** Close the focused auxiliary pane and promote its sibling. The conversation remains mounted. */
export function closePane(layout: PaneLayout): PaneLayout {
  const active = paneLeaves(layout.root).find((pane) => pane.id === layout.active)
  if (!active || active.kind === "conversation") throw new Error("Cannot close the conversation pane; select an auxiliary pane")
  const remove = (node: PaneNode): PaneNode => {
    if (node.type === "pane") return node
    if (paneLeaves(node.first).some((pane) => pane.id === active.id)) {
      if (node.first.type === "pane" && node.first.id === active.id) return node.second
      return { ...node, first: remove(node.first) }
    }
    if (node.second.type === "pane" && node.second.id === active.id) return node.first
    return { ...node, second: remove(node.second) }
  }
  const root = remove(layout.root)
  return { ...layout, root, active: paneLeaves(root)[0]?.id ?? "pane-1" }
}

interface Rect { left: number; top: number; right: number; bottom: number }

/** Normalized rectangle map; direction picking follows visible geometry, including nested splits. */
export function paneRects(node: PaneNode, rect: Rect = { left: 0, top: 0, right: 1, bottom: 1 }): Map<string, Rect> {
  if (node.type === "pane") return new Map([[node.id, rect]])
  const vertical = node.axis === "vertical"
  const cut = vertical ? rect.left + (rect.right - rect.left) * node.weight : rect.top + (rect.bottom - rect.top) * node.weight
  const first = vertical ? { ...rect, right: cut } : { ...rect, bottom: cut }
  const second = vertical ? { ...rect, left: cut } : { ...rect, top: cut }
  return new Map([...paneRects(node.first, first), ...paneRects(node.second, second)])
}

/** Move to the nearest pane whose rectangle lies in the requested direction. */
export function movePaneFocus(layout: PaneLayout, direction: PaneDirection): PaneLayout {
  const rects = paneRects(layout.root)
  const current = rects.get(layout.active)
  if (!current) return layout
  const cx = (current.left + current.right) / 2
  const cy = (current.top + current.bottom) / 2
  const candidates = [...rects].filter(([id, rect]) => {
    if (id === layout.active) return false
    const x = (rect.left + rect.right) / 2
    const y = (rect.top + rect.bottom) / 2
    return direction === "left" ? x < cx : direction === "right" ? x > cx : direction === "up" ? y < cy : y > cy
  })
  const score = (rect: Rect): number => {
    const x = (rect.left + rect.right) / 2
    const y = (rect.top + rect.bottom) / 2
    const horizontal = direction === "left" || direction === "right"
    const overlap = horizontal
      ? Math.max(0, Math.min(current.bottom, rect.bottom) - Math.max(current.top, rect.top))
      : Math.max(0, Math.min(current.right, rect.right) - Math.max(current.left, rect.left))
    return (overlap > 0 ? 0 : 2) + (horizontal ? Math.abs(x - cx) : Math.abs(y - cy)) + (horizontal ? Math.abs(y - cy) : Math.abs(x - cx)) / 10
  }
  candidates.sort((a, b) => score(a[1]) - score(b[1]))
  return candidates[0] ? { ...layout, active: candidates[0][0] } : layout
}

/** Resize the active pane against its closest sibling. Positive values grow it. */
export function resizePane(layout: PaneLayout, delta: number): PaneLayout {
  if (!Number.isFinite(delta)) throw new Error("Resize must be a finite number")
  if (paneLeaves(layout.root).length === 1) throw new Error("Split the workspace before resizing a pane")
  const resize = (node: PaneNode): [PaneNode, boolean] => {
    if (node.type === "pane") return [node, false]
    const firstHas = paneLeaves(node.first).some((pane) => pane.id === layout.active)
    const secondHas = paneLeaves(node.second).some((pane) => pane.id === layout.active)
    if ((firstHas && node.first.type === "pane") || (secondHas && node.second.type === "pane")) {
      return [{ ...node, weight: Math.max(0.1, Math.min(0.9, node.weight + (firstHas ? delta : -delta))) }, true]
    }
    if (firstHas) { const [first, done] = resize(node.first); return [{ ...node, first }, done] }
    if (secondHas) { const [second, done] = resize(node.second); return [{ ...node, second }, done] }
    return [node, false]
  }
  return { ...layout, root: resize(layout.root)[0] }
}

const rightSidebarKinds: readonly PaneKind[] = ["sessions", "todos", "context"]

/**
 * The weight `split` is drawn with at `columns` wide. A vertical split whose
 * second side holds only right-sidebar panes (Sessions, Todos, Context)
 * leaves that side at least `sidebarMinColumns`; the conversation keeps
 * 10% however narrow the split. The saved weight is not changed.
 */
export function renderedWeight(split: PaneSplit, columns: number): number {
  if (split.axis !== "vertical" || columns <= 0 || !paneLeaves(split.second).every((pane) => rightSidebarKinds.includes(pane.kind))) return split.weight
  return Math.min(split.weight, Math.max(0.1, 1 - sidebarMinColumns / Math.floor(columns)))
}

/**
 * The weight that puts `split`'s boundary (the first column of its second
 * side) at screen column `pointer`, for a split drawn from column `left`,
 * `columns` wide: within the 10–90% a saved layout allows and never
 * narrower than `renderedWeight` draws the right sidebar.
 */
export function boundaryWeight(split: PaneSplit, left: number, columns: number, pointer: number): number {
  const weight = Math.max(0.1, Math.min(0.9, (pointer - left) / columns))
  return renderedWeight({ ...split, weight }, columns)
}

/**
 * Set the weight of the split that separates pane `first` (on its first
 * side) from pane `second` (on its second side) in the saved tree. The same
 * layout when no split separates them that way.
 */
export function setSplitWeight(layout: PaneLayout, first: string, second: string, weight: number): PaneLayout {
  const has = (node: PaneNode, id: string): boolean => paneLeaves(node).some((pane) => pane.id === id)
  const update = (node: PaneNode): PaneNode => {
    if (node.type === "pane") return node
    if (has(node.first, first) && has(node.second, second)) return node.weight === weight ? node : { ...node, weight }
    const target = has(node.first, first) ? "first" : "second"
    const child = update(node[target])
    return child === node[target] ? node : { ...node, [target]: child }
  }
  const root = update(layout.root)
  return root === layout.root ? layout : { ...layout, root }
}

function migrateLegacyDefault(root: PaneNode): PaneNode {
  if (root.type !== "split" || root.axis !== "vertical" || root.weight !== 0.18 || root.first.type !== "pane" || root.first.kind !== "projects") return root
  const center = root.second
  if (center.type !== "split" || center.axis !== "vertical" || center.weight !== 0.74 || center.first.type !== "pane" || center.first.kind !== "conversation") return root
  return { ...root, weight: 0.1, second: { ...center, weight: 0.88 } }
}

/** Validate a preference file's untrusted JSON before it reaches the layout store. */
export function parsePaneLayout(value: unknown): PaneLayout | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined
  const record = value as Record<string, unknown>
  if ((record.version !== 1 && record.version !== 2) || typeof record.active !== "string") return undefined
  const ids = new Set<string>()
  let conversations = 0
  const valid = (node: unknown, depth: number): node is PaneNode => {
    if (depth > 15 || typeof node !== "object" || node === null || Array.isArray(node)) return false
    const row = node as Record<string, unknown>
    if (row.type === "pane") {
      if (typeof row.id !== "string" || !/^pane-[1-9]\d*$/.test(row.id) || ids.has(row.id) || !paneKinds.includes(row.kind as PaneKind)) return false
      ids.add(row.id)
      if (row.kind === "conversation") conversations += 1
      return ids.size <= maxPanes
    }
    if (row.type !== "split" || (row.axis !== "horizontal" && row.axis !== "vertical") || typeof row.weight !== "number" || !Number.isFinite(row.weight) || row.weight < 0.1 || row.weight > 0.9) return false
    return valid(row.first, depth + 1) && valid(row.second, depth + 1)
  }
  if (!valid(record.root, 0) || conversations !== 1 || !ids.has(record.active)) return undefined
  if (record.version === 2) return { version: 2, root: migrateLegacyDefault(record.root), active: record.active }
  if (ids.size > maxPanes - 4) return undefined
  // Version 1 saved only the center. Preserve that subtree and add the old sidebars as editable leaves.
  let next = Math.max(...[...ids].map((id) => Number(id.slice(5))))
  const leaf = (kind: PaneKind): PaneLeaf => ({ type: "pane", id: `pane-${++next}`, kind })
  const right: PaneNode = { type: "split", axis: "horizontal", weight: 0.62, first: leaf("sessions"), second: {
    type: "split", axis: "horizontal", weight: 0.36, first: leaf("todos"), second: leaf("context"),
  } }
  return { version: 2, active: record.active, root: { type: "split", axis: "vertical", weight: 0.1, first: leaf("projects"), second: {
    type: "split", axis: "vertical", weight: 0.88, first: record.root, second: right,
  } } }
}
