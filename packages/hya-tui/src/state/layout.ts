/**
 * Layout rules: one main column (header, transcript, pending block, status
 * line, composer, footer) plus a sidebar on the right.
 *
 * The sidebar mode starts `auto`: shown when the terminal is at least
 * `layoutBreakpoints.sidebar` columns wide. Toggling (Ctrl+B, `/sidebar`)
 * pins it `open` or `closed` at any width. Pure functions; the store keeps
 * the mode and the terminal width.
 */

export type SidebarMode = "auto" | "open" | "closed"

/** Terminal widths (columns) at which the layout changes. */
export const layoutBreakpoints = { sidebar: 110, projectsSidebar: 150 } as const

const sidebarMax = 32
const sidebarMin = 20
const projectsSidebarMax = 28
const projectsSidebarMin = 16

export function sidebarVisible(mode: SidebarMode, columns: number): boolean {
  if (mode === "auto") return columns >= layoutBreakpoints.sidebar
  return mode === "open"
}

/** The pinned mode after a toggle: the opposite of what is visible now. */
export function toggledSidebar(mode: SidebarMode, columns: number): SidebarMode {
  return sidebarVisible(mode, columns) ? "closed" : "open"
}

/** Sidebar width: 32 columns, at most 40% of a narrow terminal, never below 20. */
export function sidebarWidth(columns: number): number {
  return Math.max(sidebarMin, Math.min(sidebarMax, Math.floor(columns * 0.4)))
}

/**
 * The left Projects sidebar (docs/tui.md "Projects"): shown automatically
 * only once both sidebars and the chat column fit
 * (`layoutBreakpoints.projectsSidebar`, wider than the right sidebar's own
 * threshold), so at ~80 columns it stays hidden even when pinned `auto`.
 * Pinning it `open` shows it at any width, same as the right sidebar.
 */
export function projectsSidebarVisible(mode: SidebarMode, columns: number): boolean {
  if (mode === "auto") return columns >= layoutBreakpoints.projectsSidebar
  return mode === "open"
}

/** The pinned mode after a toggle: the opposite of what is visible now. */
export function toggledProjectsSidebar(mode: SidebarMode, columns: number): SidebarMode {
  return projectsSidebarVisible(mode, columns) ? "closed" : "open"
}

/** Left sidebar width: narrower than the right one (names and a count, not a session tree). */
export function projectsSidebarWidth(columns: number): number {
  return Math.max(projectsSidebarMin, Math.min(projectsSidebarMax, Math.floor(columns * 0.3)))
}

/** `on`/`show` → true, `off`/`hide` → false, no argument → the opposite of `current`. */
export function parseSwitch(argument: string | undefined, current: boolean, usage = "Usage: [on|off]"): boolean {
  switch ((argument ?? "").toLowerCase()) {
    case "": return !current
    case "on": case "show": case "open": return true
    case "off": case "hide": case "close": return false
    default: throw new Error(usage)
  }
}
