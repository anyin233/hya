/** Local split tree for the whole workspace. Backend projections are shared by its panes. */
import { projectsSidebarVisible, sidebarMinColumns, sidebarVisible, type SidebarMode } from "./layout"

export const paneKinds = ["conversation", "composer", "activity", "projects", "jobs", "sessions", "todos", "context", "models", "workflows", "interactions", "status", "api"] as const
export type PaneKind = typeof paneKinds[number]
export interface PaneDefinition {
  title: string
  selectable: boolean
  minColumns: number
  minRows: number
}
/** Layout and input eligibility share one definition for each pane kind. */
export const paneDefinitions: Record<PaneKind, PaneDefinition> = {
  conversation: { title: "Conversation", selectable: false, minColumns: 12, minRows: 3 },
  composer: { title: "Message editor", selectable: true, minColumns: 12, minRows: 3 },
  activity: { title: "Agent activity", selectable: false, minColumns: 8, minRows: 1 },
  projects: { title: "Projects", selectable: true, minColumns: 8, minRows: 3 },
  sessions: { title: "Sessions", selectable: true, minColumns: 8, minRows: 3 },
  jobs: { title: "Jobs", selectable: true, minColumns: 8, minRows: 3 },
  todos: { title: "Todos", selectable: false, minColumns: 8, minRows: 2 },
  context: { title: "Context", selectable: false, minColumns: 8, minRows: 2 },
  status: { title: "Status", selectable: false, minColumns: 8, minRows: 2 },
  models: { title: "Models", selectable: true, minColumns: 8, minRows: 3 },
  workflows: { title: "Workflows", selectable: true, minColumns: 8, minRows: 3 },
  interactions: { title: "Interactions", selectable: true, minColumns: 8, minRows: 3 },
  api: { title: "API", selectable: true, minColumns: 8, minRows: 3 },
}
export function isSelectablePane(pane: PaneLeaf): boolean { return paneDefinitions[pane.kind].selectable }
export function selectablePanes(root: PaneNode): PaneLeaf[] { return paneLeaves(root).filter(isSelectablePane) }
export function normalizePaneFocus(layout: PaneLayout): PaneLayout {
  const panes = selectablePanes(layout.root)
  const active = panes.find((pane) => pane.id === layout.active)?.id
    ?? panes.find((pane) => pane.kind === "composer")?.id ?? panes[0]?.id
  return !active || active === layout.active ? layout : { ...layout, active }
}
export function rotatePaneFocus(layout: PaneLayout, direction: 1 | -1 = 1): PaneLayout {
  const panes = selectablePanes(layout.root)
  if (!panes.length) return layout
  const index = panes.findIndex((pane) => pane.id === layout.active)
  return { ...layout, active: panes[(Math.max(0, index) + direction + panes.length) % panes.length]!.id }
}
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
  /** Horizontal docks can size one child to its visible content; absent means weighted. */
  sizing?: "weighted" | "content-first" | "content-second"
  /** Fraction of available width/height allocated to `first`. */
  weight: number
  first: PaneNode
  second: PaneNode
}

export type PaneNode = PaneLeaf | PaneSplit
export interface PaneLayout {
  version: 3
  root: PaneNode
  active: string
}

export const maxPanes = 32

export function defaultPaneLayout(): PaneLayout {
  const leaf = (id: number, kind: PaneKind): PaneLeaf => ({ type: "pane", id: `pane-${id}`, kind })
  return { version: 3, active: "pane-1", root: {
    type: "split", axis: "vertical", weight: 0.1, first: leaf(2, "projects"), second: {
      type: "split", axis: "vertical", weight: 0.88, first: {
        type: "split", axis: "horizontal", sizing: "content-second", weight: 0.8, first: leaf(6, "conversation"), second: {
          type: "split", axis: "horizontal", sizing: "content-first", weight: 0.2, first: leaf(7, "activity"), second: leaf(1, "composer"),
        },
      }, second: {
        type: "split", axis: "horizontal", weight: 0.62, first: leaf(3, "sessions"), second: {
          type: "split", axis: "horizontal", weight: 0.36, first: leaf(4, "todos"), second: leaf(5, "context"),
        },
      },
    },
  } }
}

/** The familiar startup arrangement, independent of which pane is selected. */
export function isDefaultPaneTree(layout: PaneLayout): boolean {
  return JSON.stringify(layout.root) === JSON.stringify(defaultPaneLayout().root)
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
  return keep(root) ?? { type: "pane", id: "pane-1", kind: "composer" }
}

/** Keep focus on a pane that remains visible after responsive filtering. */
export function visiblePaneLayout(layout: PaneLayout, columns: number, sidebar: SidebarMode, projectsSidebar: SidebarMode): PaneLayout {
  const root = visiblePaneRoot(layout.root, columns, sidebar, projectsSidebar)
  return normalizePaneFocus({ ...layout, root })
}

export function paneLeaves(node: PaneNode): PaneLeaf[] {
  return node.type === "pane" ? [node] : [...paneLeaves(node.first), ...paneLeaves(node.second)]
}

function mapPane(node: PaneNode, id: string, change: (pane: PaneLeaf) => PaneNode): PaneNode {
  if (node.type === "pane") return node.id === id ? change(node) : node
  return { ...node, first: mapPane(node.first, id, change), second: mapPane(node.second, id, change) }
}

/** Explicit structural edits restore proportional sizing along the edited branch. */
function weightedPaneAncestors(node: PaneNode, id: string): PaneNode {
  if (node.type === "pane" || !paneLeaves(node).some((pane) => pane.id === id)) return node
  return { ...node, ...(node.sizing && node.sizing !== "weighted" ? { sizing: "weighted" as const } : {}), first: weightedPaneAncestors(node.first, id), second: weightedPaneAncestors(node.second, id) }
}

/** Split the active rectangle into two equal rectangles; the new pane gets focus. */
export function splitPane(layout: PaneLayout, axis: PaneAxis, kind: PaneKind = "jobs", before = false): PaneLayout {
  const leaves = paneLeaves(layout.root)
  if (leaves.length >= maxPanes) throw new Error(`A layout supports at most ${maxPanes} panes`)
  if (kind === "conversation" || kind === "composer") throw new Error(`Use /layout assign ${kind} to move the ${kind} pane`)
  const next = Math.max(...leaves.map((leaf) => Number(leaf.id.match(/^pane-(\d+)$/)?.[1] ?? 0))) + 1
  const id = `pane-${next}`
  return {
    ...layout,
    root: mapPane(weightedPaneAncestors(layout.root, layout.active), layout.active, (pane) => {
      const added: PaneLeaf = { type: "pane", id, kind }
      return { type: "split", axis, weight: 0.5, first: before ? added : pane, second: before ? pane : added }
    }),
    active: paneDefinitions[kind].selectable ? id : layout.active,
  }
}

/** Assign a job; singleton viewer/editor assignments swap their existing leaves. */
export function setPaneKind(layout: PaneLayout, kind: PaneKind): PaneLayout {
  const leaves = paneLeaves(layout.root)
  const active = leaves.find((leaf) => leaf.id === layout.active)
  if (!active || active.kind === kind) return layout
  const singleton = kind === "conversation" || kind === "composer"
  if (!singleton && (active.kind === "conversation" || active.kind === "composer")) throw new Error(`Move ${active.kind} to another pane before assigning this pane`)
  const existing = singleton ? leaves.find((leaf) => leaf.kind === kind) : undefined
  const root = existing ? mapPane(layout.root, existing.id, (pane) => ({ ...pane, kind: active.kind })) : layout.root
  return normalizePaneFocus({ ...layout, root: mapPane(root, active.id, (pane) => ({ ...pane, kind })) })
}

/** Close the selected pane or an explicit kind/id; preserve focus when removing another pane. */
export function closePane(layout: PaneLayout, target?: string): PaneLayout {
  const leaves = paneLeaves(layout.root)
  const matches = target === undefined ? leaves.filter((pane) => pane.id === layout.active)
    : leaves.filter((pane) => pane.id === target || pane.kind === target)
  if (!matches.length) throw new Error(`Unknown pane: ${target ?? layout.active}`)
  if (matches.length > 1) throw new Error(`Pane ${target} is ambiguous; use a pane id: ${matches.map((pane) => pane.id).join(", ")}`)
  const active = matches[0]!
  if (active.kind === "conversation" || active.kind === "composer") throw new Error("Cannot close the conversation or composer pane; choose an auxiliary pane")
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
  return normalizePaneFocus({ ...layout, root })
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

/** A subtree with one input owner navigates as a group, including its passive siblings. */
export function paneFocusRects(root: PaneNode): Map<string, Rect> {
  const physical = paneRects(root)
  const rects = new Map(physical)
  const visit = (node: PaneNode): string[] => {
    if (node.type === "pane") return isSelectablePane(node) ? [node.id] : []
    const ids = [...visit(node.first), ...visit(node.second)]
    if (ids.length === 1) {
      const bounds = paneLeaves(node).map((pane) => physical.get(pane.id)!)
      rects.set(ids[0]!, { left: Math.min(...bounds.map((r) => r.left)), top: Math.min(...bounds.map((r) => r.top)), right: Math.max(...bounds.map((r) => r.right)), bottom: Math.max(...bounds.map((r) => r.bottom)) })
    }
    return ids
  }
  visit(root)
  return rects
}

/** Move to the nearest pane whose rectangle lies in the requested direction. */
export function movePaneFocus(layout: PaneLayout, direction: PaneDirection): PaneLayout {
  const rects = paneFocusRects(layout.root)
  const eligible = new Set(selectablePanes(layout.root).map((pane) => pane.id))
  const current = rects.get(layout.active)
  if (!current) return layout
  const cx = (current.left + current.right) / 2
  const cy = (current.top + current.bottom) / 2
  const candidates = [...rects].filter(([id, rect]) => {
    if (id === layout.active || !eligible.has(id)) return false
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
  return { ...layout, root: weightedPaneAncestors(resize(layout.root)[0], layout.active) }
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

/** Upgrade only the generated viewer/activity/editor dock; custom weights remain weighted. */
function migrateConversationDock(node: PaneNode): PaneNode {
  if (node.type === "pane") return node
  const first = migrateConversationDock(node.first)
  const second = migrateConversationDock(node.second)
  if (node.sizing === undefined && node.axis === "horizontal" && node.weight === 0.8
    && first.type === "pane" && first.kind === "conversation"
    && second.type === "split" && second.sizing === undefined && second.axis === "horizontal" && second.weight === 0.2
    && second.first.type === "pane" && second.first.kind === "activity"
    && second.second.type === "pane" && second.second.kind === "composer") {
    return { ...node, sizing: "content-second", first, second: { ...second, sizing: "content-first" } }
  }
  return first === node.first && second === node.second ? node : { ...node, first, second }
}

/** Validate a preference file's untrusted JSON before it reaches the layout store. */
export function parsePaneLayout(value: unknown): PaneLayout | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined
  const record = value as Record<string, unknown>
  if ((record.version !== 1 && record.version !== 2 && record.version !== 3) || typeof record.active !== "string") return undefined
  const ids = new Set<string>()
  let conversations = 0
  let composers = 0
  const valid = (node: unknown, depth: number): node is PaneNode => {
    if (depth > 31 || typeof node !== "object" || node === null || Array.isArray(node)) return false
    const row = node as Record<string, unknown>
    if (row.type === "pane") {
      if (typeof row.id !== "string" || !/^pane-[1-9]\d*$/.test(row.id) || ids.has(row.id) || !paneKinds.includes(row.kind as PaneKind)) return false
      ids.add(row.id)
      if (row.kind === "conversation") conversations += 1
      if (row.kind === "composer") composers += 1
      return ids.size <= maxPanes
    }
    if (row.type !== "split" || (row.axis !== "horizontal" && row.axis !== "vertical") || typeof row.weight !== "number" || !Number.isFinite(row.weight) || row.weight < 0.1 || row.weight > 0.9) return false
    if (row.sizing !== undefined && row.sizing !== "weighted" && row.sizing !== "content-first" && row.sizing !== "content-second") return false
    if ((row.sizing === "content-first" || row.sizing === "content-second") && row.axis !== "horizontal") return false
    return valid(row.first, depth + 1) && valid(row.second, depth + 1)
  }
  if (!valid(record.root, 0) || conversations !== 1 || !ids.has(record.active)) return undefined
  if (record.version === 3) {
    if (composers !== 1) return undefined
    return normalizePaneFocus({ version: 3, root: migrateConversationDock(record.root), active: record.active })
  }
  if (composers !== 0) return undefined
  // Keep the old conversation id on the editor so saved shortcuts still target input.
  let next = Math.max(...[...ids].map((id) => Number(id.slice(5))))
  const leaf = (kind: PaneKind): PaneLeaf => ({ type: "pane", id: `pane-${++next}`, kind })
  let root = migrateLegacyDefault(record.root)
  if (record.version === 1) {
    if (ids.size > maxPanes - 6) return undefined
    const right: PaneNode = { type: "split", axis: "horizontal", weight: 0.62, first: leaf("sessions"), second: {
      type: "split", axis: "horizontal", weight: 0.36, first: leaf("todos"), second: leaf("context"),
    } }
    root = { type: "split", axis: "vertical", weight: 0.1, first: leaf("projects"), second: {
      type: "split", axis: "vertical", weight: 0.88, first: root, second: right,
    } }
  } else if (ids.size > maxPanes - 2) return undefined
  const conversation = paneLeaves(root).find((pane) => pane.kind === "conversation")!
  root = mapPane(root, conversation.id, (pane) => ({ type: "split", axis: "horizontal", sizing: "content-second", weight: 0.8,
    first: leaf("conversation"), second: { type: "split", axis: "horizontal", sizing: "content-first", weight: 0.2,
      first: leaf("activity"), second: { ...pane, kind: "composer" },
    },
  }))
  return normalizePaneFocus({ version: 3, root, active: record.active })
}
