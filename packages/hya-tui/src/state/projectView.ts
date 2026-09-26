/**
 * The full-screen Project view (`/project`, `/projects`; docs/tui.md
 * "Projects"), opened over the RulesView pattern (state/rules.ts):
 * list every Project, open/switch one, create one (name then one root per
 * step, primary = first), edit an existing one's roots (add/remove/reorder),
 * rename, delete (with the server's `failed_precondition` message shown
 * verbatim when a live session blocks it), and start a temporary session.
 * Pure state and keys; the calls live in app/projectView.ts.
 */
import { HttpError, type ProjectInfo } from "../client"
import type { KeyLike } from "../keys/bindings"
import { stripTerminalControls } from "../sanitize"

/**
 * A failed call as one status line, `<code>: <message>`: the server's
 * error (`HttpError.detail`, without the method and path), or
 * `unavailable: <reason>` when the request never got an answer (a fetch
 * failure). Only the first line is kept, so a stack never reaches the
 * screen.
 */
export function errorLine(error: unknown): string {
  const text = error instanceof HttpError
    ? error.detail
    : error instanceof TypeError
      ? `unavailable: ${error.message}`
      : error instanceof Error ? error.message : String(error)
  const line = text.split(/\r?\n/).map((part) => stripTerminalControls(part).trim()).find(Boolean)
  return line ?? "unknown error"
}

export interface ProjectViewBusy {
  label: string
  startedAt: number
}

export interface ProjectViewNotice {
  text: string
  tone: "info" | "ok" | "error"
}

/** The name-then-roots prompt for a new Project (`n`). */
export interface CreateFlow {
  step: "name" | "root"
  name: string
  roots: string[]
  input: string
}

/** The rename prompt (`r`). */
export interface RenameFlow {
  id: string
  input: string
}

/** The roots editor (`e`): Up/Down select a root, `a` adds one (text input), `d` removes the selected one (refused when it is the only root), Up/Down with Shift reorders (Shift+Up moves the selected root earlier; the first root is primary), Enter commits, Esc cancels. */
export interface EditRootsFlow {
  id: string
  roots: string[]
  selected: number
  /** A text input is open to add a root. */
  adding?: boolean
  input: string
}

export interface ProjectViewState {
  /** Highlighted row in the Project list. */
  highlighted: string | undefined
  busy?: ProjectViewBusy
  notice?: ProjectViewNotice
  /** Set while `d` asks to confirm a delete. */
  confirm?: string
  create?: CreateFlow
  rename?: RenameFlow
  editRoots?: EditRootsFlow
}

export type ProjectViewOutcome =
  | { type: "none" }
  | { type: "update"; view: ProjectViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "switch"; id: string }
  | { type: "createStart" }
  | { type: "createRoot"; name: string; roots: string[] }
  | { type: "rename"; id: string; title: string }
  | { type: "editRootsCommit"; id: string; roots: string[] }
  | { type: "delete"; id: string }
  | { type: "temporary" }

export function initialProjectView(projects: readonly ProjectInfo[], activeId: string | undefined): ProjectViewState {
  return { highlighted: activeId ?? projects[0]?.id }
}

/** Keep the highlight on its row after a reload (the first row when it is gone). */
export function settleProjectView(view: ProjectViewState, projects: readonly ProjectInfo[]): ProjectViewState {
  if (!projects.length) return { ...view, highlighted: undefined }
  return projects.some((project) => project.id === view.highlighted) ? view : { ...view, highlighted: projects[0]!.id }
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"
const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

function move(view: ProjectViewState, projects: readonly ProjectInfo[], step: number): ProjectViewState {
  if (!projects.length) return view
  const at = Math.max(0, projects.findIndex((project) => project.id === view.highlighted))
  return { ...view, highlighted: projects[(at + step + projects.length) % projects.length]!.id }
}

/** Editing a plain text field (create/rename/add-root inputs): Enter/Esc/backspace/typing. */
function editText(input: string, key: KeyLike): { text?: string; commit?: boolean; cancel?: boolean } {
  if (key.name === "escape") return { cancel: true }
  if (isEnter(key)) return { commit: true }
  if (key.name === "backspace") return { text: input.slice(0, -1) }
  if (printable(key)) return { text: input + key.sequence }
  return {}
}

/** One key while the Project view is open (components/Composer.tsx routes it like the Saved Rules view). */
export function projectViewKey(view: ProjectViewState, key: KeyLike, projects: readonly ProjectInfo[]): ProjectViewOutcome {
  if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }

  if (view.create) {
    const flow = view.create
    const result = editText(flow.input, key)
    if (result.cancel) return { type: "update", view: { ...view, create: undefined } }
    if (result.text !== undefined) return { type: "update", view: { ...view, create: { ...flow, input: result.text } } }
    if (result.commit) {
      if (flow.step === "name") {
        const name = flow.input.trim()
        if (!name) return { type: "update", view: { ...view, notice: { tone: "info", text: "Name cannot be empty" } } }
        return { type: "update", view: { ...view, create: { step: "root", name, roots: [], input: "" } } }
      }
      const root = flow.input.trim()
      if (root) return { type: "update", view: { ...view, create: { ...flow, roots: [...flow.roots, root], input: "" } } }
      if (!flow.roots.length) return { type: "update", view: { ...view, notice: { tone: "info", text: "At least one root is required" } } }
      return { type: "createRoot", name: flow.name, roots: flow.roots }
    }
    return { type: "none" }
  }

  if (view.rename) {
    const flow = view.rename
    const result = editText(flow.input, key)
    if (result.cancel) return { type: "update", view: { ...view, rename: undefined } }
    if (result.text !== undefined) return { type: "update", view: { ...view, rename: { ...flow, input: result.text } } }
    if (result.commit) {
      const title = flow.input.trim()
      if (!title) return { type: "update", view: { ...view, notice: { tone: "info", text: "Name cannot be empty" } } }
      return { type: "rename", id: flow.id, title }
    }
    return { type: "none" }
  }

  if (view.editRoots) {
    const flow = view.editRoots
    if (flow.adding) {
      const result = editText(flow.input, key)
      if (result.cancel) return { type: "update", view: { ...view, editRoots: { ...flow, adding: false, input: "" } } }
      if (result.text !== undefined) return { type: "update", view: { ...view, editRoots: { ...flow, input: result.text } } }
      if (result.commit) {
        const root = flow.input.trim()
        if (!root) return { type: "update", view: { ...view, editRoots: { ...flow, adding: false, input: "" } } }
        return { type: "update", view: { ...view, editRoots: { ...flow, roots: [...flow.roots, root], selected: flow.roots.length, adding: false, input: "" } } }
      }
      return { type: "none" }
    }
    if (key.name === "escape") return { type: "update", view: { ...view, editRoots: undefined } }
    if (isEnter(key)) return { type: "editRootsCommit", id: flow.id, roots: flow.roots }
    if (key.name === "up" && !key.shift) return { type: "update", view: { ...view, editRoots: { ...flow, selected: Math.max(0, flow.selected - 1) } } }
    if (key.name === "down" && !key.shift) return { type: "update", view: { ...view, editRoots: { ...flow, selected: Math.min(flow.roots.length - 1, flow.selected + 1) } } }
    if (key.name === "up" && key.shift && flow.selected > 0) {
      const roots = [...flow.roots]
      const [row] = roots.splice(flow.selected, 1)
      roots.splice(flow.selected - 1, 0, row!)
      return { type: "update", view: { ...view, editRoots: { ...flow, roots, selected: flow.selected - 1 } } }
    }
    if (key.name === "down" && key.shift && flow.selected < flow.roots.length - 1) {
      const roots = [...flow.roots]
      const [row] = roots.splice(flow.selected, 1)
      roots.splice(flow.selected + 1, 0, row!)
      return { type: "update", view: { ...view, editRoots: { ...flow, roots, selected: flow.selected + 1 } } }
    }
    if (key.sequence === "a") return { type: "update", view: { ...view, editRoots: { ...flow, adding: true, input: "" } } }
    if (key.sequence === "d") {
      if (flow.roots.length <= 1) return { type: "update", view: { ...view, notice: { tone: "info", text: "A Project needs at least one root" } } }
      const roots = flow.roots.filter((_, index) => index !== flow.selected)
      return { type: "update", view: { ...view, editRoots: { ...flow, roots, selected: Math.min(flow.selected, roots.length - 1) } } }
    }
    return { type: "none" }
  }

  if (view.confirm) {
    if (key.name === "escape") return { type: "update", view: { ...view, confirm: undefined } }
    return isEnter(key) ? { type: "delete", id: view.confirm } : { type: "none" }
  }

  if (key.name === "up") return { type: "update", view: move(view, projects, -1) }
  if (key.name === "down") return { type: "update", view: move(view, projects, 1) }
  if (key.name === "escape") return { type: "close" }
  if (isEnter(key)) return view.highlighted ? { type: "switch", id: view.highlighted } : { type: "none" }
  if (key.ctrl || key.meta) return { type: "none" }
  if (key.sequence === "n") return { type: "update", view: { ...view, notice: undefined, create: { step: "name", name: "", roots: [], input: "" } } }
  if (key.sequence === "t") return { type: "temporary" }
  if (key.sequence === "r") {
    if (!view.highlighted) return { type: "none" }
    const project = projects.find((row) => row.id === view.highlighted)
    return { type: "update", view: { ...view, notice: undefined, rename: { id: view.highlighted, input: project?.name ?? "" } } }
  }
  if (key.sequence === "e") {
    if (!view.highlighted) return { type: "none" }
    const project = projects.find((row) => row.id === view.highlighted)
    if (!project) return { type: "none" }
    return { type: "update", view: { ...view, notice: undefined, editRoots: { id: project.id, roots: [...project.roots], selected: 0, input: "" } } }
  }
  if (key.sequence === "d") {
    if (!view.highlighted) return { type: "none" }
    return { type: "update", view: { ...view, notice: undefined, confirm: view.highlighted } }
  }
  return { type: "none" }
}

/** The footer hint for the current screen or sub-flow. */
export function projectViewHint(view: ProjectViewState): string {
  if (view.busy) return `${view.busy.label}… · Esc cancels`
  if (view.create) return view.create.step === "name" ? "Type a name · Enter continues · Esc cancels" : "Type a root path · Enter adds it · Enter on empty finishes · Esc cancels"
  if (view.rename) return "Type a new name · Enter renames · Esc cancels"
  if (view.editRoots) {
    if (view.editRoots.adding) return "Type a root path · Enter adds it · Esc cancels"
    return "Up/Down select · Shift+Up/Down reorder (first = primary) · a add · d remove · Enter saves · Esc cancels"
  }
  if (view.confirm) return "Enter deletes · Esc cancels"
  return "Up/Down move · Enter opens/switches · n new · e edit roots · r rename · d delete · t temporary session · Esc close"
}
