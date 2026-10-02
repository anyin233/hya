/** Per-instance, transient tree editor state. All edits use the shared layout reducers. */
import type { KeyLike } from "../keys/bindings"
import { closePane, insertPane, movePane, paneDefinitions, paneKinds, paneLeaves, paneNodes, resolvePaneNode, setPaneKind, setPaneSize, wrapPane, type PaneKind, type PaneLayout, type PaneNode, type PaneSize, type PaneSplit } from "./panes"

export interface LayoutTreeRow { id: string; node: PaneNode; depth: number; parent?: PaneSplit; index: number; size?: PaneSize }
export function layoutTreeRows(root: PaneNode): LayoutTreeRow[] {
  const rows: LayoutTreeRow[] = []
  const visit = (node: PaneNode, depth: number, index: number, parent?: PaneSplit, size?: PaneSize) => {
    rows.push({ id: node.id, node, depth, index, parent, size })
    if (node.type === "split") node.children.forEach((child, at) => visit(child.node, depth + 1, at, node, child.size))
  }
  visit(root, 0, 0)
  return rows
}
type KindAction = "before" | "after" | "child" | "assign" | "row" | "column"
export type LayoutEditorStage = { type: "tree" | "actions" | "destination" | "remove" }
  | { type: "kind"; action: KindAction }
  | { type: "position"; destination: string }
  | { type: "weight"; value: string; replace: boolean }
export interface LayoutEditorState { selected: string; stage: LayoutEditorStage; index: number; error?: string }
export interface LayoutEditorRow { id: string; label: string; detail: string; depth?: number }
export interface LayoutEditorOutcome { state: LayoutEditorState; layout?: PaneLayout }
export const createLayoutEditor = (selected: string): LayoutEditorState => ({ selected, stage: { type: "tree" }, index: 0 })
const protectedNode = (node: PaneNode) => paneLeaves(node).some((pane) => pane.kind === "conversation" || pane.kind === "composer")
export const paneSizeText = (size?: PaneSize) => !size ? "root" : size.mode === "content" ? "content" : `weight ${Number(size.value.toPrecision(6))}`

/** External layout edits can remove the selection or a pending destination. */
export function reconcileLayoutEditor(layout: PaneLayout, state: LayoutEditorState): LayoutEditorState {
  const nodes = paneNodes(layout.root)
  if (!nodes.some((node) => node.id === state.selected)) return createLayoutEditor(layout.root.id)
  if (state.stage.type === "position" && !nodes.some((node) => node.id === (state.stage as { destination: string }).destination && node.type === "split")) return createLayoutEditor(state.selected)
  return state
}
export function layoutEditorRows(layout: PaneLayout, state: LayoutEditorState): LayoutEditorRow[] {
  const tree = layoutTreeRows(layout.root), selected = tree.find((row) => row.id === state.selected) ?? tree[0]!
  const { node, parent } = selected
  switch (state.stage.type) {
    case "tree": return tree.map((row) => ({ id: row.id, depth: row.depth,
      label: `${row.node.type === "split" ? "▾" : "·"} ${row.id} ${row.node.type === "split" ? row.node.direction : row.node.kind}`,
      detail: `${paneSizeText(row.size)} · ${row.node.type === "split" ? `${row.node.children.length} children` : paneDefinitions[row.node.kind].selectable ? "selectable" : "passive"}` }))
    case "actions": return [
      ...(node.type === "split" ? [{ id: "child", label: "Add child", detail: "Append a pane to this container" }] : []),
      ...(parent ? [
        { id: "before", label: "Insert before", detail: "Add a pane before this node" },
        { id: "after", label: "Insert after", detail: "Add a pane after this node" },
        { id: "weight", label: "Change weight", detail: `${paneSizeText(selected.size)}${parent.direction === "column" ? " · or content" : ""}` },
        { id: "move", label: "Move", detail: "Choose a container and insertion position" },
      ] : []),
      ...(node.type === "pane" && !protectedNode(node) ? [{ id: "assign", label: "Change job", detail: "Assign another auxiliary pane kind" }] : []),
      { id: "row", label: "Wrap in row", detail: "Add a pane to the left of this subtree" },
      { id: "column", label: "Wrap in column", detail: "Add a pane above this subtree" },
      { id: "remove", label: "Remove", detail: protectedNode(node) ? "Protected: contains the conversation or editor" : `Remove ${paneLeaves(node).length} pane(s) from the layout` },
    ]
    case "kind": return paneKinds.filter((kind) => kind !== "conversation" && kind !== "composer").map((kind) => ({ id: kind, label: kind, detail: paneDefinitions[kind].title }))
    case "destination": {
      const descendants = new Set(paneNodes(node).map((item) => item.id))
      return tree.filter((row) => row.node.type === "split" && !descendants.has(row.id)).map((row) => ({ id: row.id, label: `${row.id} ${row.node.type === "split" ? row.node.direction : ""}`, detail: "Move into this container" }))
    }
    case "position": {
      const destination = resolvePaneNode(layout, state.stage.destination)
      if (destination.type !== "split") return []
      const children = destination.children.filter((child) => child.node.id !== node.id)
      return [...children.map((child, index) => ({ id: String(index), label: `Before ${child.node.id}`, detail: child.node.type === "split" ? child.node.direction : child.node.kind })),
        { id: String(children.length), label: "At end", detail: `Position ${children.length}` }]
    }
    case "remove": return [{ id: "cancel", label: "Cancel", detail: "Keep this node" }, { id: "remove", label: "Remove node", detail: `${node.id} and its ${paneLeaves(node).length} pane(s)` }]
    case "weight": return []
  }
}
export function layoutEditorHeading(state: LayoutEditorState): string {
  switch (state.stage.type) {
    case "tree": return "Layout tree"
    case "actions": return `Edit ${state.selected}`
    case "kind": return "Choose pane job"
    case "destination": return "Move into container"
    case "position": return `Position in ${state.stage.destination}`
    case "remove": return `Remove ${state.selected}?`
    case "weight": return `Weight for ${state.selected}`
  }
}
export function layoutEditorChoose(layout: PaneLayout, original: LayoutEditorState, id: string): LayoutEditorOutcome {
  const state = reconcileLayoutEditor(layout, original)
  const selected = layoutTreeRows(layout.root).find((row) => row.id === state.selected)!
  const go = (stage: LayoutEditorStage): LayoutEditorOutcome => ({ state: { selected: state.selected, stage, index: 0 } })
  const done = (next: PaneLayout, id = state.selected): LayoutEditorOutcome => ({ layout: next, state: reconcileLayoutEditor(next, createLayoutEditor(id)) })
  try {
    if (!layoutEditorRows(layout, state).some((row) => row.id === id)) return { state }
    switch (state.stage.type) {
      case "tree": return { state: createLayoutEditor(id) }
      case "actions":
        switch (id) {
          case "weight": return go({ type: "weight", value: selected.size?.mode === "weight" ? String(selected.size.value) : "content", replace: true })
          case "move": return go({ type: "destination" })
          case "remove":
            if (protectedNode(selected.node)) throw new Error("Cannot remove the conversation or message editor")
            return go({ type: "remove" })
          default: return go({ type: "kind", action: id as KindAction })
        }
      case "kind": {
        const kind = id as PaneKind
        switch (state.stage.action) {
          case "child": return done(insertPane(layout, state.selected, selected.node.type === "split" ? selected.node.children.length : 0, kind))
          case "before": case "after":
            if (!selected.parent) throw new Error("The root has no sibling position")
            return done(insertPane(layout, selected.parent.id, selected.index + (state.stage.action === "after" ? 1 : 0), kind))
          case "assign": return done(setPaneKind({ ...layout, active: state.selected }, kind))
          case "row": case "column": return done(wrapPane(layout, state.selected, state.stage.action, kind))
        }
      }
      case "destination": return go({ type: "position", destination: id })
      case "position": return done(movePane(layout, state.selected, state.stage.destination, Number(id)))
      case "remove": return id === "cancel" ? go({ type: "tree" }) : done(closePane(layout, state.selected), selected.parent?.id)
      case "weight": return { state }
    }
  } catch (error) { return { state: { ...state, error: error instanceof Error ? error.message : String(error) } } }
}
export function layoutEditorBack(state: LayoutEditorState): LayoutEditorState {
  return { selected: state.selected, index: 0, stage: { type: state.stage.type === "actions" || state.stage.type === "tree" ? "tree" : "actions" } }
}
export function layoutEditorKey(layout: PaneLayout, original: LayoutEditorState, key: KeyLike): LayoutEditorOutcome {
  const state = reconcileLayoutEditor(layout, original)
  if (key.ctrl || key.meta || key.option || key.super) return { state }
  if (key.shift && state.stage.type !== "weight") return { state }
  if (key.name === "escape") return { state: layoutEditorBack(state) }
  if (state.stage.type === "weight") {
    const stage = state.stage
    if (key.name === "return" || key.name === "kpenter") {
      try {
        const value = stage.value.trim()
        const size: PaneSize = value === "content" ? { mode: "content" } : { mode: "weight", value: value ? Number(value) : NaN }
        const next = setPaneSize(layout, state.selected, size)
        return { layout: next, state: reconcileLayoutEditor(next, createLayoutEditor(state.selected)) }
      } catch (error) { return { state: { ...state, error: error instanceof Error ? error.message : String(error) } } }
    }
    if (key.name === "backspace" || key.name === "delete") return { state: { ...state, error: undefined, stage: { ...stage, replace: false, value: stage.replace ? "" : stage.value.slice(0, -1) } } }
    if (key.sequence.length === 1 && /^[\x20-\x7e]$/.test(key.sequence)) return layoutEditorPaste(state, key.sequence)
    return { state }
  }
  const rows = layoutEditorRows(layout, state)
  const index = state.stage.type === "tree" ? rows.findIndex((row) => row.id === state.selected) : Math.min(state.index, Math.max(0, rows.length - 1))
  const chooseIndex = (at: number): LayoutEditorOutcome => state.stage.type === "tree" ? { state: createLayoutEditor(rows[at]?.id ?? state.selected) } : { state: { ...state, index: at, error: undefined } }
  switch (key.name) {
    case "up": return chooseIndex(Math.max(0, index - 1))
    case "down": return chooseIndex(Math.min(rows.length - 1, index + 1))
    case "home": return chooseIndex(0)
    case "end": return chooseIndex(rows.length - 1)
    case "left": case "right": {
      if (state.stage.type !== "tree") return { state }
      const row = layoutTreeRows(layout.root).find((row) => row.id === state.selected)!
      const target = key.name === "left" ? row.parent?.id : row.node.type === "split" ? row.node.children[0]?.node.id : undefined
      return target ? { state: createLayoutEditor(target) } : { state }
    }
    case "return": case "kpenter":
      if (state.stage.type === "tree") return { state: { ...state, stage: { type: "actions" }, index: 0, error: undefined } }
      return rows[index] ? layoutEditorChoose(layout, state, rows[index]!.id) : { state }
    default: return { state }
  }
}
export function layoutEditorPaste(state: LayoutEditorState, text: string): LayoutEditorOutcome {
  if (state.stage.type !== "weight") return { state }
  const cleaned = text.replace(/[\x00-\x1f\x7f]/g, "")
  return { state: { ...state, error: undefined, stage: { ...state.stage, replace: false, value: ((state.stage.replace ? "" : state.stage.value) + cleaned).slice(0, 64) } } }
}
