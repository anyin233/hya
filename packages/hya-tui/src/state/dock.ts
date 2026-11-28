/**
 * The dock above the input (ADR-0036 `todos` and `subagents` components,
 * docs/tui-config.md "Dock"): open todos and busy child sessions, one borderless
 * row each, between the pending block and the prompt. The todo rows yield
 * to the visible workspace Todos pane. The rows and their count
 * for the layout.
 */
import { extensionManager } from "../extensions/manager"
import { todoGlyphs, todoStatusText } from "./format"
import { paneLeaves, visiblePaneRoot, type PaneKind } from "./panes"
import { treeSessionIds, waitingKind } from "./prompts"
import type { AppState } from "./store"

export interface DockRow {
  glyph: string
  text: string
  tone: "activity" | "muted" | "warning" | "success"
}

/** Busy child session rows shown at most; the rest fold into `+N more`. */
export const maxChildRows = 3

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
 * Busy or waiting descendant sessions of the open session, at most
 * `maxChildRows`; empty when the component is off.
 */
export function childDockRows(state: AppState): DockRow[] {
  const root = state.selected?.id
  if (!root || !state.structure["subagents"].enabled) return []
  const tree = treeSessionIds(root, state.sessions)
  const live = state.sessions
    .filter((session) => session.id !== root && tree.has(session.id) && !session.archived)
    .flatMap((session) => {
      const waiting = waitingKind(state.interactions, session.id)
      if (!waiting && !session.busy) return []
      return [{
        label: session.title || session.agent || session.id,
        waiting: Boolean(waiting),
      }]
    })
  const rows = live.slice(0, live.length > maxChildRows ? maxChildRows - 1 : maxChildRows).map((row): DockRow => ({
    glyph: "↳",
    text: `${row.label}${row.waiting ? " · waiting for an answer" : ""}`,
    tone: row.waiting ? "warning" : "activity",
  }))
  if (live.length > rows.length) rows.push({ glyph: "…", text: `+${live.length - rows.length} more`, tone: "muted" })
  return rows
}

/** Rows the dock's todo and child session groups take (the layout reserves them). */
export function dockRowCount(state: AppState, columns: number): number {
  return todoDockRows(state, columns).length + childDockRows(state).length
}
