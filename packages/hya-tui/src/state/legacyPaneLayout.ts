/** Read-only v1-v3 preference migration. No rendering or editing uses this binary model. */
import { paneKinds, paneDefinitions, maxPanes, type PaneKind } from "./panes"
export type PaneAxis = "horizontal" | "vertical"
export interface PaneLeaf { type: "pane"; id: string; kind: PaneKind }
export interface PaneSplit { type: "split"; axis: PaneAxis; sizing?: "weighted" | "content-first" | "content-second"; weight: number; first: PaneNode; second: PaneNode }
export type PaneNode = PaneLeaf | PaneSplit
export interface PaneLayout { version: 3; root: PaneNode; active: string }
function paneLeaves(node: PaneNode): PaneLeaf[] { return node.type === "pane" ? [node] : [...paneLeaves(node.first), ...paneLeaves(node.second)] }
function mapPane(node: PaneNode, id: string, change: (pane: PaneLeaf) => PaneNode): PaneNode {
  return node.type === "pane" ? node.id === id ? change(node) : node : { ...node, first: mapPane(node.first, id, change), second: mapPane(node.second, id, change) }
}
function normalizePaneFocus(layout: PaneLayout): PaneLayout {
  const panes = paneLeaves(layout.root).filter((pane) => paneDefinitions[pane.kind].selectable)
  const active = panes.find((pane) => pane.id === layout.active)?.id ?? panes.find((pane) => pane.kind === "composer")?.id ?? panes[0]?.id
  return !active || active === layout.active ? layout : { ...layout, active }
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
export function parseLegacyPaneLayout(value: unknown): PaneLayout | undefined {
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
      if (typeof row.id !== "string" || !Number.isSafeInteger(Number(row.id.slice(5))) || !/^pane-[1-9]\d*$/.test(row.id) || ids.has(row.id) || !paneKinds.includes(row.kind as PaneKind)) return false
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
