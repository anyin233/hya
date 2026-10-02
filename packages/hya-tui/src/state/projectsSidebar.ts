/**
 * The left Projects sidebar (docs/tui.md "Projects"): every Project, live via
 * `projectsUpdated` (the same refresh that feeds `state.projects`). Shown
 * automatically once both sidebars and the chat column fit
 * (`layoutBreakpoints.projectsSidebar`), toggled with `/projects-sidebar` or
 * a browser-safe key; a focused sidebar takes Up/Down/Enter to switch.
 * Pure helpers; the store keeps the mode and focus, app/controller.ts owns
 * `switchProject`.
 */
import type { ProjectInfo } from "../client"
import type { KeyLike } from "../keys/bindings"

export interface ProjectSidebarRow {
  id: string
  name: string
  /** `ProjectInfo.busy`: a session of it runs a turn now. */
  busy: boolean
  sessionCount: number
  active: boolean
}

/** One row per Project, in list order, tagged with the active one and its live busy/session-count fields. */
export function projectSidebarRows(projects: readonly ProjectInfo[], activeId: string | undefined): ProjectSidebarRow[] {
  return projects.map((project) => ({
    id: project.id,
    name: project.name,
    busy: project.busy === true,
    sessionCount: project.sessionCount ?? 0,
    active: project.id === activeId,
  }))
}

/** One rendered line of the sidebar: a Project row, or the rule drawn between Projects. */
export interface ProjectSidebarEntry {
  row?: ProjectSidebarRow
  separator?: boolean
}

/**
 * The sidebar's Project list with separators, the same shape as the Sessions
 * list (state/format.ts `sessionListEntries`): one row per Project with a
 * rule between Projects; none before the first or after the last.
 */
export function projectSidebarEntries(projects: readonly ProjectInfo[], activeId: string | undefined): ProjectSidebarEntry[] {
  const entries: ProjectSidebarEntry[] = []
  for (const row of projectSidebarRows(projects, activeId)) {
    if (entries.length) entries.push({ separator: true })
    entries.push({ row })
  }
  return entries
}

export type ProjectsSidebarOutcome =
  | { type: "none" }
  | { type: "move"; id: string }
  | { type: "switch"; id: string }
  | { type: "blur" }

const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

/**
 * One key while the left sidebar has focus: Up/Down move the highlight,
 * Enter switches to the highlighted Project, Esc returns focus to the
 * composer without switching.
 */
export function projectsSidebarKey(key: KeyLike, rows: readonly ProjectSidebarRow[], highlighted: string | undefined): ProjectsSidebarOutcome {
  if (key.name === "escape") return { type: "blur" }
  if (!rows.length) return { type: "none" }
  const at = Math.max(0, rows.findIndex((row) => row.id === highlighted))
  if (key.name === "up") return { type: "move", id: rows[(at - 1 + rows.length) % rows.length]!.id }
  if (key.name === "down") return { type: "move", id: rows[(at + 1) % rows.length]!.id }
  if (isEnter(key)) return { type: "switch", id: rows[at]!.id }
  return { type: "none" }
}
