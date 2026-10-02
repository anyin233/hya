/** Local split tree for the whole workspace. Backend projections are shared by its panes. */
import { projectsSidebarVisible, sidebarMinColumns, sidebarVisible, type SidebarMode } from "./layout"

export const paneKinds = ["conversation", "composer", "activity", "projects", "jobs", "sessions", "todos", "context", "models", "workflows", "interactions", "status", "api", "layout"] as const
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
  jobs: { title: "Jobs", selectable: true, minColumns: 8, minRows: 5 },
  todos: { title: "Todos", selectable: false, minColumns: 8, minRows: 2 },
  context: { title: "Context", selectable: false, minColumns: 8, minRows: 2 },
  status: { title: "Status", selectable: false, minColumns: 8, minRows: 2 },
  models: { title: "Models", selectable: true, minColumns: 8, minRows: 5 },
  workflows: { title: "Workflows", selectable: true, minColumns: 8, minRows: 5 },
  interactions: { title: "Interactions", selectable: true, minColumns: 8, minRows: 3 },
  api: { title: "API", selectable: true, minColumns: 8, minRows: 3 },
  layout: { title: "Layout tree", selectable: true, minColumns: 24, minRows: 10 },
}

import { parseLegacyPaneLayout, type PaneNode as LegacyNode } from "./legacyPaneLayout"

export type PaneAxis = "horizontal" | "vertical"
export type PaneDirection = "left" | "right" | "up" | "down"
export type LayoutDirection = "row" | "column"
export interface PaneLeaf { type: "pane"; id: string; kind: PaneKind }
export type PaneSize = { mode: "weight"; value: number } | { mode: "content" }
export interface PaneChild { node: PaneNode; size: PaneSize }
export interface PaneSplit { type: "split"; id: string; direction: LayoutDirection; children: PaneChild[] }
export type PaneNode = PaneLeaf | PaneSplit
export interface PaneLayout { version: 4; root: PaneNode; active: string }
export interface Rect { left: number; top: number; right: number; bottom: number }
export const maxPanes = 32
const weighted = (node: PaneNode, value = 1): PaneChild => ({ node, size: { mode: "weight", value: Math.max(Number.MIN_VALUE, value) } })
const content = (node: PaneNode): PaneChild => ({ node, size: { mode: "content" } })
export function paneNodes(node: PaneNode): PaneNode[] { return node.type === "pane" ? [node] : [node, ...node.children.flatMap((child) => paneNodes(child.node))] }
export function paneLeaves(node: PaneNode): PaneLeaf[] { return paneNodes(node).filter((node): node is PaneLeaf => node.type === "pane") }
export function isSelectablePane(pane: PaneLeaf): boolean { return paneDefinitions[pane.kind].selectable }
export function selectablePanes(root: PaneNode): PaneLeaf[] { return paneLeaves(root).filter(isSelectablePane) }
export function normalizePaneFocus(layout: PaneLayout): PaneLayout {
  const panes = selectablePanes(layout.root)
  const active = panes.find((pane) => pane.id === layout.active)?.id ?? panes.find((pane) => pane.kind === "composer")?.id ?? panes[0]?.id
  return !active || active === layout.active ? layout : { ...layout, active }
}

/** Flatten compatible equal-direction branches without changing their relative sizes. */
export function normalizePaneNode(node: PaneNode): PaneNode {
  if (node.type === "pane") return node
  const children: PaneChild[] = []
  for (const child of node.children) {
    const nested = normalizePaneNode(child.node)
    if (nested.type === "split" && nested.children.length === 0) continue
    if (nested.type === "split" && nested.direction === node.direction) {
      if (child.size.mode === "weight" && nested.children.every((item) => item.size.mode === "weight")) {
        const total = nested.children.reduce((sum, item) => sum + (item.size.mode === "weight" ? item.size.value : 0), 0)
        children.push(...nested.children.map((item) => weighted(item.node, child.size.mode === "weight" && item.size.mode === "weight" ? child.size.value * (item.size.value / total) : 1)))
        continue
      }
      if (child.size.mode === "content") {
        children.push(...nested.children.map((item) => content(item.node)))
        continue
      }
    }
    children.push(nested === child.node ? child : { ...child, node: nested })
  }
  if (children.length === 1) return children[0]!.node
  return children.length === node.children.length && children.every((child, index) => child === node.children[index]) ? node : { ...node, children }
}
function finish(layout: PaneLayout, root: PaneNode, active = layout.active): PaneLayout {
  return normalizePaneFocus({ version: 4, root: normalizePaneNode(root), active })
}
export function defaultPaneLayout(): PaneLayout {
  const leaf = (id: number, kind: PaneKind): PaneLeaf => ({ type: "pane", id: `pane-${id}`, kind })
  return { version: 4, active: "pane-1", root: { type: "split", id: "group-1", direction: "row", children: [
    weighted(leaf(2, "projects"), .1),
    weighted({ type: "split", id: "group-2", direction: "column", children: [weighted(leaf(6, "conversation")), content(leaf(7, "activity")), content(leaf(1, "composer"))] }, .792),
    weighted({ type: "split", id: "group-3", direction: "column", children: [weighted(leaf(3, "sessions"), .62), weighted(leaf(4, "todos"), .1368), weighted(leaf(5, "context"), .2432)] }, .108),
  ] } }
}
export function isDefaultPaneTree(layout: PaneLayout): boolean { return JSON.stringify(layout.root) === JSON.stringify(defaultPaneLayout().root) }
export function visiblePaneRoot(root: PaneNode, columns: number, sidebar: SidebarMode, projectsSidebar: SidebarMode): PaneNode {
  const right = sidebarVisible(sidebar, columns), left = projectsSidebarVisible(projectsSidebar, columns)
  const keep = (node: PaneNode): PaneNode | undefined => {
    if (node.type === "pane") return (node.kind === "projects" && !left) || (["sessions", "todos", "context"].includes(node.kind) && !right) ? undefined : node
    const children = node.children.flatMap((child) => { const kept = keep(child.node); return kept ? [kept === child.node ? child : { ...child, node: kept }] : [] })
    if (!children.length) return undefined
    if (children.length === 1) return children[0]!.node
    return children.every((child, i) => child === node.children[i]) && children.length === node.children.length ? node : { ...node, children }
  }
  return keep(root) ?? { type: "pane", id: "pane-1", kind: "composer" }
}
export function visiblePaneLayout(layout: PaneLayout, columns: number, sidebar: SidebarMode, projectsSidebar: SidebarMode): PaneLayout { return normalizePaneFocus({ ...layout, root: visiblePaneRoot(layout.root, columns, sidebar, projectsSidebar) }) }
function mapNode(node: PaneNode, id: string, change: (node: PaneNode) => PaneNode): PaneNode {
  if (node.id === id) return change(node)
  if (node.type === "pane") return node
  const children = node.children.map((child) => { const next = mapNode(child.node, id, change); return next === child.node ? child : { ...child, node: next } })
  return children.every((child, i) => child === node.children[i]) ? node : { ...node, children }
}
export function resolvePaneNode(layout: PaneLayout, target: string): PaneNode {
  if (target === "root") return layout.root
  const matches = paneNodes(layout.root).filter((node) => node.id === target || (node.type === "pane" && node.kind === target))
  if (!matches.length) throw new Error(`Unknown pane or container: ${target}`)
  if (matches.length > 1) throw new Error(`Pane ${target} is ambiguous; use a pane id: ${matches.map((node) => node.id).join(", ")}`)
  return matches[0]!
}
function nextId(root: PaneNode, prefix: "pane" | "group"): string {
  const maximum = Math.max(0, ...paneNodes(root).map((node) => node.id.startsWith(`${prefix}-`) ? Number(node.id.slice(prefix.length + 1)) : 0))
  if (maximum >= Number.MAX_SAFE_INTEGER) throw new Error(`${prefix} id space exhausted`)
  return `${prefix}-${maximum + 1}`
}
function newPane(layout: PaneLayout, kind: PaneKind): PaneLeaf {
  if (paneLeaves(layout.root).length >= maxPanes) throw new Error(`A layout supports at most ${maxPanes} panes`)
  if (kind === "conversation" || kind === "composer") throw new Error(`Use /layout assign ${kind} to move the ${kind} pane`)
  return { type: "pane", id: nextId(layout.root, "pane"), kind }
}
function checkIndex(index: number, count: number): void { if (!Number.isInteger(index) || index < 0 || index > count) throw new Error(`Insertion index must be an integer from 0 to ${count}`) }
export function insertPane(layout: PaneLayout, container: string, index: number, kind: PaneKind): PaneLayout {
  const parent = resolvePaneNode(layout, container)
  if (parent.type !== "split") throw new Error(`Target ${container} is not a container`)
  checkIndex(index, parent.children.length)
  const added = newPane(layout, kind)
  const root = mapNode(layout.root, parent.id, (node) => node.type === "split" ? { ...node, children: [...node.children.slice(0, index), weighted(added), ...node.children.slice(index)] } : node)
  return finish(layout, root, isSelectablePane(added) ? added.id : layout.active)
}
export function wrapPane(layout: PaneLayout, target: string, direction: LayoutDirection, kind: PaneKind, before = true): PaneLayout {
  const selected = resolvePaneNode(layout, target)
  const added = newPane(layout, kind)
  const children = before ? [weighted(added), weighted(selected)] : [weighted(selected), weighted(added)]
  const root = mapNode(layout.root, selected.id, () => ({ type: "split", id: nextId(layout.root, "group"), direction, children }))
  return finish(layout, root, isSelectablePane(added) ? added.id : layout.active)
}
/** Split inserts into a matching parent, otherwise wraps the selected leaf. */
export function splitPane(layout: PaneLayout, axis: PaneAxis, kind: PaneKind = "jobs", before = false): PaneLayout {
  const direction = axis === "vertical" ? "row" : "column"
  const parent = paneNodes(layout.root).find((node): node is PaneSplit => node.type === "split" && node.children.some((child) => child.node.id === layout.active))
  if (parent?.direction === direction) {
    const index = parent.children.findIndex((child) => child.node.id === layout.active)
    const added = newPane(layout, kind), selected = parent.children[index]!
    // Divide the selected slot, preserving the proportions of all its siblings.
    const value = selected.size.mode === "weight" ? selected.size.value / 2 : .5
    const pair = [weighted(added, value), weighted(selected.node, value)]
    if (!before) pair.reverse()
    const root = mapNode(layout.root, parent.id, (node) => node.type === "split" ? { ...node, children: [...node.children.slice(0, index), ...pair, ...node.children.slice(index + 1)] } : node)
    return finish(layout, root, isSelectablePane(added) ? added.id : layout.active)
  }
  return wrapPane(layout, layout.active, direction, kind, before)
}
export function setPaneKind(layout: PaneLayout, kind: PaneKind): PaneLayout {
  const active = paneLeaves(layout.root).find((pane) => pane.id === layout.active)
  if (!active || active.kind === kind) return layout
  const singleton = kind === "conversation" || kind === "composer"
  if (!singleton && (active.kind === "conversation" || active.kind === "composer")) throw new Error(`Move ${active.kind} to another pane before assigning this pane`)
  const existing = singleton ? paneLeaves(layout.root).find((pane) => pane.kind === kind) : undefined
  let root = existing ? mapNode(layout.root, existing.id, (node) => node.type === "pane" ? { ...node, kind: active.kind } : node) : layout.root
  root = mapNode(root, active.id, (node) => node.type === "pane" ? { ...node, kind } : node)
  return finish(layout, root)
}
function detach(node: PaneNode, id: string): PaneNode | undefined {
  if (node.id === id) return undefined
  if (node.type === "pane") return node
  return { ...node, children: node.children.flatMap((child) => { const next = detach(child.node, id); return next ? [{ ...child, node: next }] : [] }) }
}
export function closePane(layout: PaneLayout, target = layout.active): PaneLayout {
  const selected = resolvePaneNode(layout, target)
  if (paneLeaves(selected).some((pane) => pane.kind === "conversation" || pane.kind === "composer")) throw new Error("Cannot close the conversation or composer pane; choose an auxiliary pane or container")
  return finish(layout, detach(layout.root, selected.id)!)
}
export function movePane(layout: PaneLayout, target: string, container: string, index: number): PaneLayout {
  const selected = resolvePaneNode(layout, target), parent = resolvePaneNode(layout, container)
  if (parent.type !== "split") throw new Error(`Target ${container} is not a container`)
  if (paneNodes(selected).some((node) => node.id === parent.id)) throw new Error("Cannot move a node into itself or its descendant")
  const originalParent = paneNodes(layout.root).find((node): node is PaneSplit => node.type === "split" && node.children.some((child) => child.node.id === selected.id))
  if (!originalParent) throw new Error("Cannot move the root into another container")
  const slot = originalParent.children.find((child) => child.node.id === selected.id)!
  const without = detach(layout.root, selected.id)!
  const remaining = paneNodes(without).find((node) => node.id === parent.id) as PaneSplit
  checkIndex(index, remaining.children.length) // index is in the destination after removal
  const movedSlot = parent.direction === "row" && slot.size.mode === "content" ? weighted(slot.node) : slot
  const root = mapNode(without, parent.id, (node) => node.type === "split" ? { ...node, children: [...node.children.slice(0, index), movedSlot, ...node.children.slice(index)] } : node)
  return finish(layout, root)
}
export function resizePane(layout: PaneLayout, delta: number): PaneLayout {
  if (!Number.isFinite(delta)) throw new Error("Resize must be a finite number")
  const parent = paneNodes(layout.root).find((node): node is PaneSplit => node.type === "split" && node.children.some((child) => child.node.id === layout.active))
  if (!parent) throw new Error("Split the workspace before resizing a pane")
  const index = parent.children.findIndex((child) => child.node.id === layout.active)
  const sibling = index === parent.children.length - 1 ? index - 1 : index + 1
  const values = parent.children.map((child) => child.size.mode === "weight" ? child.size.value : 1)
  const total = values.reduce((a, b) => a + b, 0), pair = values[index]! + values[sibling]!
  const chosen = Math.max(pair * .1, Math.min(pair * .9, values[index]! + delta * total))
  return finish(layout, mapNode(layout.root, parent.id, (node) => node.type === "split" ? { ...node, children: node.children.map((child, i) => weighted(child.node, i === index ? chosen : i === sibling ? pair - chosen : values[i]!)) } : node))
}
/** Set one child's relative allocation; unrelated sibling sizes are preserved. */
export function setPaneSize(layout: PaneLayout, target: string, size: PaneSize): PaneLayout {
  const selected = resolvePaneNode(layout, target)
  const parent = paneNodes(layout.root).find((node): node is PaneSplit => node.type === "split" && node.children.some((child) => child.node.id === selected.id))
  if (!parent) throw new Error("The root has no parent weight")
  if (size.mode === "content" && parent.direction !== "column") throw new Error("Content sizing is available only in a column")
  if (size.mode === "weight" && (!Number.isFinite(size.value) || size.value <= 0)) throw new Error("Weight must be a positive finite number")
  const children = parent.children.map((child) => child.node.id === selected.id ? { ...child, size } : child)
  if (!Number.isFinite(children.reduce((sum, child) => sum + (child.size.mode === "weight" ? child.size.value : 0), 0))) throw new Error("Total container weight must be finite")
  return finish(layout, mapNode(layout.root, parent.id, (node) => node.type === "split" ? { ...node, children } : node))
}

/** Opening the editor reuses an existing instance, or adds a full-height column. */
export function openLayoutPane(layout: PaneLayout): PaneLayout {
  const existing = paneLeaves(layout.root).find((pane) => pane.kind === "layout")
  return existing ? { ...layout, active: existing.id } : wrapPane(layout, "root", "row", "layout")
}
/** Change a visible pair's boundary, retaining its total weight and skipped slots. */
export function setContainerBoundary(layout: PaneLayout, container: string, index: number, ratio: number, secondIndex = index + 1): PaneLayout {
  if (!Number.isFinite(ratio)) throw new Error("Boundary ratio must be finite")
  const parent = resolvePaneNode(layout, container)
  if (parent.type !== "split" || index < 0 || secondIndex <= index || secondIndex >= parent.children.length || !Number.isInteger(index) || !Number.isInteger(secondIndex)) throw new Error("Unknown container boundary")
  const left = parent.children[index]!, right = parent.children[secondIndex]!
  const total = (left.size.mode === "weight" ? left.size.value : 1) + (right.size.mode === "weight" ? right.size.value : 1)
  const fraction = Math.max(.1, Math.min(.9, ratio))
  return { ...layout, root: mapNode(layout.root, parent.id, (node) => node.type === "split" ? { ...node, children: node.children.map((child, i) => i === index ? weighted(child.node, total * fraction) : i === secondIndex ? weighted(child.node, total * (1 - fraction)) : child) } : node) }
}

/** Layout solver also returns container bounds for structural editing and dragging. */
export function layoutRects(root: PaneNode, rect: Rect, minimum: (node: PaneNode, direction: LayoutDirection) => number = () => 0): Map<string, Rect> {
  const result = new Map<string, Rect>()
  const visit = (node: PaneNode, bounds: Rect): void => {
    result.set(node.id, bounds)
    if (node.type === "pane") return
    const row = node.direction === "row", length = row ? bounds.right - bounds.left : bounds.bottom - bounds.top
    const mins = node.children.map((child) => Math.max(0, minimum(child.node, node.direction)))
    const desired = node.children.map((child, i) => child.size.mode === "content" ? mins[i]! : 0)
    const free = Math.max(0, length - desired.reduce((a, b) => a + b, 0))
    const total = node.children.reduce((sum, child) => sum + (child.size.mode === "weight" ? child.size.value : 0), 0)
    node.children.forEach((child, i) => { if (child.size.mode === "weight") desired[i] = total ? free * (child.size.value / total) : 0 })
    // Enforce minimums by taking only spare space from siblings, then fit undersized screens.
    for (let i = 0; i < desired.length; i++) {
      let deficit = Math.max(0, mins[i]! - desired[i]!)
      const donors = desired.map((_, j) => j).filter((j) => j !== i).sort((a, b) => (desired[b]! - mins[b]!) - (desired[a]! - mins[a]!))
      for (const j of donors) {
        if (deficit <= 0) break
        const take = Math.min(deficit, Math.max(0, desired[j]! - mins[j]!))
        desired[j]! -= take; desired[i]! += take; deficit -= take
      }
    }
    const sum = desired.reduce((a, b) => a + b, 0)
    if (sum > length) desired.forEach((value, i) => desired[i] = value * length / sum)
    let offset = row ? bounds.left : bounds.top
    const start = offset
    let cumulative = 0
    const end = row ? bounds.right : bounds.bottom
    node.children.forEach((child, i) => {
      cumulative += desired[i]!
      const cut = i === node.children.length - 1 && (total > 0 || sum >= length) ? end : Math.max(offset, Math.min(end, Math.round(start + cumulative)))
      visit(child.node, row ? { ...bounds, left: offset, right: cut } : { ...bounds, top: offset, bottom: cut })
      offset = cut
    })
  }
  visit(root, rect)
  return result
}
export function paneRects(root: PaneNode, rect: Rect = { left: 0, top: 0, right: 1000, bottom: 1000 }): Map<string, Rect> {
  const minimum = (node: PaneNode, direction: LayoutDirection): number => node.type === "pane" ? direction === "row" ? paneDefinitions[node.kind].minColumns : node.kind === "activity" ? 0 : paneDefinitions[node.kind].minRows
    : node.direction === direction ? node.children.reduce((sum, child) => sum + minimum(child.node, direction), 0) : Math.max(...node.children.map((child) => minimum(child.node, direction)))
  return layoutRects(root, rect, minimum)
}
export function paneFocusRects(root: PaneNode): Map<string, Rect> { return paneRects(root) }
function orderedPanes(layout: PaneLayout, measured?: ReadonlyMap<string, Rect>): { id: string; rect: Rect }[] {
  const bounds = measured ?? paneRects(layout.root)
  return selectablePanes(layout.root).flatMap((pane) => { const rect = bounds.get(pane.id); return rect && rect.right > rect.left && rect.bottom > rect.top ? [{ id: pane.id, rect }] : [] })
    .sort((a, b) => a.rect.top - b.rect.top || a.rect.left - b.rect.left || a.id.localeCompare(b.id, undefined, { numeric: true }))
}
export function rotatePaneFocus(layout: PaneLayout, direction: 1 | -1 = 1, measured?: ReadonlyMap<string, Rect>): PaneLayout {
  const panes = orderedPanes(layout, measured)
  if (!panes.length) return layout
  const index = panes.findIndex((pane) => pane.id === layout.active)
  const next = panes[(index < 0 ? direction === 1 ? 0 : panes.length - 1 : (index + direction + panes.length) % panes.length)]!
  return next.id === layout.active ? layout : { ...layout, active: next.id }
}
export function movePaneFocus(layout: PaneLayout, direction: PaneDirection, measured?: ReadonlyMap<string, Rect>): PaneLayout {
  if (direction === "left" || direction === "right") return rotatePaneFocus(layout, direction === "left" ? -1 : 1, measured)
  const panes = orderedPanes(layout, measured), current = panes.find((pane) => pane.id === layout.active)?.rect
  if (!current) return layout
  const cy = (current.top + current.bottom) / 2, cx = (current.left + current.right) / 2
  const candidates = panes.filter((pane) => pane.id !== layout.active && (direction === "up" ? pane.rect.bottom <= current.top : pane.rect.top >= current.bottom))
  const score = (rect: Rect): number[] => [
    Math.min(current.right, rect.right) > Math.max(current.left, rect.left) ? 0 : 1,
    Math.max(0, direction === "up" ? current.top - rect.bottom : rect.top - current.bottom),
    Math.abs((rect.left + rect.right) / 2 - cx), Math.abs((rect.top + rect.bottom) / 2 - cy),
  ]
  candidates.sort((a, b) => { const x = score(a.rect), y = score(b.rect); for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return x[i]! - y[i]!; return a.id.localeCompare(b.id, undefined, { numeric: true }) })
  return candidates[0] ? { ...layout, active: candidates[0].id } : layout
}

/** Migrate binary splits once; their editor/viewer ids and proportional weights survive. */
function fromLegacy(root: LegacyNode): PaneNode {
  let next = 0
  const convert = (node: LegacyNode): PaneNode => {
    if (node.type === "pane") return node
    const first = convert(node.first), second = convert(node.second)
    return { type: "split", id: `group-${++next}`, direction: node.axis === "vertical" ? "row" : "column", children: [
      node.sizing === "content-first" ? content(first) : weighted(first, node.sizing === "content-second" ? 1 : node.weight),
      node.sizing === "content-second" ? content(second) : weighted(second, node.sizing === "content-first" ? 1 : 1 - node.weight),
    ] }
  }
  return normalizePaneNode(convert(root))
}
export function parsePaneLayout(value: unknown): PaneLayout | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined
  const record = value as Record<string, unknown>
  if (record.version !== 4) { const legacy = parseLegacyPaneLayout(value); return legacy ? { version: 4, root: fromLegacy(legacy.root), active: legacy.active } : undefined }
  if (typeof record.active !== "string") return undefined
  const ids = new Set<string>(); let leaves = 0, groups = 0, conversations = 0, composers = 0
  const valid = (node: unknown, depth: number): node is PaneNode => {
    if (depth > 31 || typeof node !== "object" || node === null || Array.isArray(node)) return false
    const row = node as Record<string, unknown>
    if (typeof row.id !== "string" || ids.has(row.id) || !Number.isSafeInteger(Number(row.id.split("-")[1]))) return false
    ids.add(row.id)
    if (row.type === "pane") {
      if (!/^pane-[1-9]\d*$/.test(row.id) || !paneKinds.includes(row.kind as PaneKind) || ++leaves > maxPanes) return false
      if (row.kind === "conversation") conversations++
      if (row.kind === "composer") composers++
      return true
    }
    if (row.type !== "split" || !/^group-[1-9]\d*$/.test(row.id) || (row.direction !== "row" && row.direction !== "column") || ++groups > 31 || !Array.isArray(row.children) || row.children.length < 2 || row.children.length > maxPanes) return false
    const childrenValid = row.children.every((item: unknown) => {
      if (typeof item !== "object" || item === null || Array.isArray(item)) return false
      const child = item as Record<string, unknown>, size = child.size as Record<string, unknown> | undefined
      if (!size || typeof size !== "object" || Array.isArray(size)) return false
      if (size.mode !== "content" && (size.mode !== "weight" || typeof size.value !== "number" || !Number.isFinite(size.value) || size.value <= 0)) return false
      if (size.mode === "content" && row.direction !== "column") return false
      return valid(child.node, depth + 1)
    })
    if (!childrenValid) return false
    const total = row.children.reduce((sum: number, item: unknown) => { const size = (item as PaneChild).size; return sum + (size?.mode === "weight" ? size.value : 0) }, 0)
    return childrenValid && Number.isFinite(total)
  }
  if (!valid(record.root, 0) || conversations !== 1 || composers !== 1 || !paneLeaves(record.root).some((pane) => pane.id === record.active)) return undefined
  return finish({ version: 4, root: record.root, active: record.active }, record.root)
}
