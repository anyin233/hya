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
type KindAction = "before" | "after" | "child" | "assign" | "row" | "column" | "insert"
export type LayoutEditorStage = { type: "tree" | "actions" | "destination" }
  | { type: "remove"; target?: string; direct?: boolean }
  | { type: "wrap"; target: string }
  | { type: "insert-position"; destination: string }
  | { type: "kind"; action: KindAction; target?: string; direct?: boolean; before?: boolean; destination?: string; position?: number }
  | { type: "position"; destination: string }
  | { type: "weight"; value: string; replace: boolean }
export interface LayoutEditorState { selected: string; marked?: string; stage: LayoutEditorStage; index: number; error?: string }
export interface LayoutEditorRow { id: string; label: string; detail: string; depth?: number }
export interface LayoutEditorOutcome { state: LayoutEditorState; layout?: PaneLayout }
export const createLayoutEditor = (selected: string): LayoutEditorState => ({ selected, stage: { type: "tree" }, index: 0 })
const protectedNode = (node: PaneNode) => paneLeaves(node).some((pane) => pane.kind === "conversation" || pane.kind === "composer")
export const paneSizeText = (size?: PaneSize) => !size ? "root" : size.mode === "content" ? "content" : `weight ${Number(size.value.toPrecision(6))}`

/** External layout edits can remove the selection or a pending destination. */
export function reconcileLayoutEditor(layout: PaneLayout, state: LayoutEditorState): LayoutEditorState {
  const nodes = paneNodes(layout.root)
  if (state.marked && !nodes.some((node) => node.id === state.marked)) state = { ...state, marked: undefined }
  if (!nodes.some((node) => node.id === state.selected)) return { ...createLayoutEditor(layout.root.id), marked: state.marked }
  const stage = state.stage
  if ("target" in stage && stage.target && !nodes.some((node) => node.id === stage.target)) return { ...state, stage: { type: "tree" }, index: 0, error: "The target no longer exists" }
  if ((stage.type === "insert-position" || stage.type === "kind" && stage.destination) && !nodes.some((node) => node.id === stage.destination && node.type === "split")) return { ...state, stage: { type: "tree" }, index: 0, error: "The insertion container no longer exists" }
  if (stage.type === "position" && !nodes.some((node) => node.id === stage.destination && node.type === "split")) return { ...state, stage: { type: "tree" }, index: 0 }
  return state
}
export function layoutEditorRows(layout: PaneLayout, state: LayoutEditorState): LayoutEditorRow[] {
  const tree = layoutTreeRows(layout.root), target = "target" in state.stage ? state.stage.target : undefined
  const selected = tree.find((row) => row.id === (target ?? state.selected)) ?? tree[0]!
  const { node, parent } = selected
  switch (state.stage.type) {
    case "tree": case "wrap": return tree.map((row) => ({ id: row.id, depth: row.depth,
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
    case "position": case "insert-position": {
      const destination = resolvePaneNode(layout, state.stage.destination)
      if (destination.type !== "split") return []
      const children = destination.children.filter((child) => state.stage.type === "insert-position" || child.node.id !== node.id)
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
    case "wrap": return `Wrap ${state.stage.target}`
    case "insert-position": return `Insert in ${state.stage.destination}`
    case "actions": return `Edit ${state.selected}`
    case "kind": return "Choose pane job"
    case "destination": return "Move into container"
    case "position": return `Position in ${state.stage.destination}`
    case "remove": return `Remove ${state.stage.target ?? state.selected}?`
    case "weight": return `Weight for ${state.selected}`
  }
}
/** The pending edit is visible before selecting a job; the marked target never changes insertion's cursor. */
export function layoutEditorPreview(state: LayoutEditorState): string | undefined {
  if (state.stage.type === "wrap") return `Wrap ${state.stage.target} · r row / c column`
  if (state.stage.type !== "kind" || !state.stage.direct) return undefined
  const stage = state.stage, target = stage.target ?? state.selected
  if (stage.action === "before") return `Insert here · before ${target}`
  if (stage.action === "insert") return `Insert here · ${stage.destination} position ${stage.position}`
  if (stage.action === "row" || stage.action === "column") return `Wrap ${target} in ${stage.action} · new pane ${stage.before ? "before" : "after"}`
  return undefined
}
export function layoutEditorHint(state: LayoutEditorState): string {
  switch (state.stage.type) {
    case "tree": return "↑↓ move · Shift+Enter/Space mark · i insert · w wrap · Del/Backspace remove · Enter actions"
    case "wrap": return "r row · c column · Esc cancel"
    case "kind": return `↑↓ choose pane · Enter adds${state.stage.direct && (state.stage.action === "row" || state.stage.action === "column") ? " · Tab before/after" : ""} · Esc cancel`
    case "insert-position": return "↑↓ choose position · Enter continues · Esc cancel"
    case "weight": return "Number or content · Enter saves · Esc back"
    default: return "↑↓ select · Enter choose · Esc back"
  }
}
function removeTarget(layout: PaneLayout, state: LayoutEditorState, target: string): LayoutEditorOutcome {
  const row = layoutTreeRows(layout.root).find((row) => row.id === target)!
  if (protectedNode(row.node)) throw new Error("Cannot remove the conversation or message editor")
  const next = closePane(layout, target), surviving = new Set(paneNodes(next.root).map((node) => node.id))
  const candidates = [row.parent?.children[row.index + 1]?.node, row.parent?.children[row.index - 1]?.node]
    .flatMap((node) => node ? paneNodes(node).map((item) => item.id) : [])
  candidates.push(row.parent?.id ?? next.root.id, state.selected, next.root.id)
  return { layout: next, state: reconcileLayoutEditor(next, { ...createLayoutEditor(candidates.find((id) => surviving.has(id))!), marked: state.marked }) }
}
export function layoutEditorChoose(layout: PaneLayout, original: LayoutEditorState, id: string): LayoutEditorOutcome {
  const state = reconcileLayoutEditor(layout, original)
  const target = "target" in state.stage ? state.stage.target ?? state.selected : state.selected
  const selected = layoutTreeRows(layout.root).find((row) => row.id === target)!
  const go = (stage: LayoutEditorStage): LayoutEditorOutcome => ({ state: { ...state, stage, index: 0, error: undefined } })
  const done = (next: PaneLayout, id = state.selected): LayoutEditorOutcome => ({ layout: next, state: reconcileLayoutEditor(next, { ...createLayoutEditor(id), marked: state.marked }) })
  const added = (next: PaneLayout): LayoutEditorOutcome => done(next, paneLeaves(next.root).find((pane) => !paneLeaves(layout.root).some((old) => old.id === pane.id))!.id)
  try {
    if (!layoutEditorRows(layout, state).some((row) => row.id === id)) return { state }
    switch (state.stage.type) {
      case "tree": return { state: { ...state, selected: id, error: undefined } }
      case "wrap": return { state }
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
        const finish = state.stage.direct ? added : done
        switch (state.stage.action) {
          case "child": return done(insertPane(layout, state.selected, selected.node.type === "split" ? selected.node.children.length : 0, kind))
          case "before": case "after":
            if (!selected.parent) throw new Error("The root has no sibling position")
            return finish(insertPane(layout, selected.parent.id, selected.index + (state.stage.action === "after" ? 1 : 0), kind))
          case "insert": return added(insertPane(layout, state.stage.destination!, state.stage.position!, kind))
          case "assign": return done(setPaneKind({ ...layout, active: state.selected }, kind))
          case "row": case "column": return finish(wrapPane(layout, target, state.stage.action, kind, state.stage.before ?? true))
        }
      }
      case "destination": return go({ type: "position", destination: id })
      case "insert-position": return go({ type: "kind", action: "insert", direct: true, destination: state.stage.destination, position: Number(id) })
      case "position": return done(movePane(layout, state.selected, state.stage.destination, Number(id)))
      case "remove": return id === "cancel" ? go({ type: "tree" }) : removeTarget(layout, state, target)
      case "weight": return { state }
    }
  } catch (error) { return { state: { ...state, error: error instanceof Error ? error.message : String(error) } } }
}
export function layoutEditorBack(state: LayoutEditorState): LayoutEditorState {
  const direct = state.stage.type === "wrap" || state.stage.type === "insert-position" || "direct" in state.stage && state.stage.direct
  return { ...state, marked: state.stage.type === "tree" ? undefined : state.marked, error: undefined, index: 0,
    stage: { type: direct || state.stage.type === "actions" || state.stage.type === "tree" ? "tree" : "actions" } }
}
export function layoutEditorKey(layout: PaneLayout, original: LayoutEditorState, key: KeyLike): LayoutEditorOutcome {
  const state = reconcileLayoutEditor(layout, original)
  const mark = state.stage.type === "tree" && !key.meta && !key.option && !key.super && (
    !key.ctrl && ((key.shift && (key.name === "return" || key.name === "kpenter")) || !key.shift && (key.name === "space" || key.name === "linefeed"))
    || key.ctrl && !key.shift && key.name === "j")
  if (mark) return { state: { ...state, marked: state.marked === state.selected ? undefined : state.selected, error: undefined } }
  if (key.ctrl || key.meta || key.option || key.super) return { state }
  if (key.shift && state.stage.type !== "weight") return { state }
  if (key.name === "escape") return { state: layoutEditorBack(state) }
  if (state.stage.type === "wrap") {
    const action = key.name === "r" ? "row" : key.name === "c" ? "column" : undefined
    return action ? { state: { ...state, stage: { type: "kind", action, direct: true, target: state.stage.target, before: false }, index: 0, error: undefined } } : { state }
  }
  if (state.stage.type === "kind" && state.stage.direct && (state.stage.action === "row" || state.stage.action === "column") && key.name === "tab") {
    return { state: { ...state, stage: { ...state.stage, before: !state.stage.before } } }
  }
  if (state.stage.type === "tree") {
    const target = state.marked ?? state.selected
    if (key.name === "w") return { state: { ...state, stage: { type: "wrap", target }, error: undefined } }
    if (key.name === "i") {
      const row = layoutTreeRows(layout.root).find((row) => row.id === state.selected)!
      return { state: { ...state, stage: row.parent ? { type: "kind", action: "before", direct: true } : { type: "insert-position", destination: row.id }, index: 0, error: undefined } }
    }
    if (key.name === "backspace" || key.name === "delete") {
      try {
        const node = resolvePaneNode(layout, target)
        if (protectedNode(node)) throw new Error("Cannot remove the conversation or message editor")
        return node.type === "pane" ? removeTarget(layout, state, target)
          : { state: { ...state, stage: { type: "remove", target, direct: true }, index: 0, error: undefined } }
      } catch (error) { return { state: { ...state, error: error instanceof Error ? error.message : String(error) } } }
    }
  }
  if (state.stage.type === "weight") {
    const stage = state.stage
    if (key.name === "return" || key.name === "kpenter") {
      try {
        const value = stage.value.trim()
        const size: PaneSize = value === "content" ? { mode: "content" } : { mode: "weight", value: value ? Number(value) : NaN }
        const next = setPaneSize(layout, state.selected, size)
        return { layout: next, state: reconcileLayoutEditor(next, { ...createLayoutEditor(state.selected), marked: state.marked }) }
      } catch (error) { return { state: { ...state, error: error instanceof Error ? error.message : String(error) } } }
    }
    if (key.name === "backspace" || key.name === "delete") return { state: { ...state, error: undefined, stage: { ...stage, replace: false, value: stage.replace ? "" : stage.value.slice(0, -1) } } }
    if (key.sequence.length === 1 && /^[\x20-\x7e]$/.test(key.sequence)) return layoutEditorPaste(state, key.sequence)
    return { state }
  }
  const rows = layoutEditorRows(layout, state)
  const index = state.stage.type === "tree" ? rows.findIndex((row) => row.id === state.selected) : Math.min(state.index, Math.max(0, rows.length - 1))
  const chooseIndex = (at: number): LayoutEditorOutcome => state.stage.type === "tree" ? { state: { ...state, selected: rows[at]?.id ?? state.selected, error: undefined } } : { state: { ...state, index: at, error: undefined } }
  switch (key.name) {
    case "up": return chooseIndex(Math.max(0, index - 1))
    case "down": return chooseIndex(Math.min(rows.length - 1, index + 1))
    case "home": return chooseIndex(0)
    case "end": return chooseIndex(rows.length - 1)
    case "left": case "right": {
      if (state.stage.type !== "tree") return { state }
      const row = layoutTreeRows(layout.root).find((row) => row.id === state.selected)!
      const target = key.name === "left" ? row.parent?.id : row.node.type === "split" ? row.node.children[0]?.node.id : undefined
      return target ? { state: { ...state, selected: target, error: undefined } } : { state }
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
