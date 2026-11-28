/**
 * The dock above the input (ADR-0036 `todos` and `subagents` components,
 * docs/tui-config.md "Dock"): open todos and running subagents, one borderless
 * row each, between the pending block and the prompt. A row group shows only
 * with something to say, and never while a visible pane of the workspace
 * already shows the same facts (deduplication). Pure: rows and their count
 * for the layout.
 */
import { extensionManager } from "../extensions/manager"
import { todoGlyphs, todoStatusText } from "./format"
import { paneLeaves, visiblePaneRoot, type PaneKind } from "./panes"
import type { AppState } from "./store"
import { subagentRows } from "./subagents"

export interface DockRow {
  glyph: string
  text: string
  tone: "activity" | "muted" | "warning" | "success"
}

/** Running subagent rows shown at most; the rest fold into `+N more`. */
export const maxSubagentRows = 3

/** Whether a pane of `kind` is drawn right now (the workspace's sidebars). */
function paneVisible(state: AppState, columns: number, kind: PaneKind): boolean {
  if (!state.structure.sidebar.enabled) return false
  return paneLeaves(visiblePaneRoot(state.paneLayout.root, columns, state.sidebar, state.projectsSidebar, extensionManager.visiblePaneOptions())).some((pane) => pane.kind === kind)
}

const toneOf = (status: string): DockRow["tone"] => status === "in_progress" ? "activity" : status === "blocked" ? "warning" : status === "completed" ? "success" : "muted"

/**
 * Open todos (in progress first, then pending and blocked), at most
 * `todos.max_rows` rows, the last one `+N more · /todos` when they do not fit.
 * Empty when the component is off, every item is done, or a visible Todos
 * pane shows them.
 */
export function todoDockRows(state: AppState, columns: number): DockRow[] {
  const settings = state.structure.todos
  if (!settings.enabled || paneVisible(state, columns, "todos")) return []
  const open = state.todos.map((item) => ({ ...item, status: todoStatusText(item.status) })).filter((item) => item.status !== "completed")
  if (!open.length) return []
  const done = state.todos.length - open.length
  const order = (status: string) => status === "in_progress" ? 0 : status === "pending" ? 1 : 2
  const sorted = [...open].sort((left, right) => order(left.status) - order(right.status))
  const fits = sorted.length <= settings.max_rows ? sorted : sorted.slice(0, settings.max_rows - 1)
  const rows = fits.map((item): DockRow => ({ glyph: todoGlyphs[item.status] ?? "·", text: item.content, tone: toneOf(item.status) }))
  const hidden = sorted.length - fits.length
  if (hidden) rows.push({ glyph: "…", text: `+${hidden} more${done ? ` · ${done} done` : ""} · /todos`, tone: "muted" })
  else if (done) rows[rows.length - 1] = { ...rows[rows.length - 1]!, text: `${rows[rows.length - 1]!.text}  (${done} done)` }
  return rows
}

/**
 * Subagents of the open session that are starting, running, or waiting for an
 * answer, at most `maxSubagentRows`; empty when the component is off or a
 * visible Subagents pane lists them.
 */
export function subagentDockRows(state: AppState, columns: number): DockRow[] {
  if (!state.structure.subagents.enabled || paneVisible(state, columns, "subagents")) return []
  const live = subagentRows(state, { includeArchived: false }).filter((row) => row.status === "running" || row.status === "starting" || row.status === "waiting")
  const rows = live.slice(0, live.length > maxSubagentRows ? maxSubagentRows - 1 : maxSubagentRows).map((row): DockRow => ({
    glyph: "↳",
    text: `${row.handle || row.label}${row.status === "waiting" ? " · waiting for an answer" : row.activity ? ` · ${row.activity}` : row.status === "starting" ? " · starting" : ""}`,
    tone: row.status === "waiting" ? "warning" : "activity",
  }))
  if (live.length > rows.length) rows.push({ glyph: "…", text: `+${live.length - rows.length} more · /subagents`, tone: "muted" })
  return rows
}

/** Rows the dock's todo and subagent groups take (the layout reserves them). */
export function dockRowCount(state: AppState, columns: number): number {
  return todoDockRows(state, columns).length + subagentDockRows(state, columns).length
}
