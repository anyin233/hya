/** Sidebar visibility modes for filtering jobs from the full workspace tree. */

export type SidebarMode = "auto" | "open" | "closed"

/** Terminal widths (columns) at which the layout changes. */
export const layoutBreakpoints = { sidebar: 110, projectsSidebar: 150 } as const

/**
 * The right sidebar (Sessions, Todos, Context) needs `layoutBreakpoints.sidebar`
 * columns: below it it is always hidden and the top status line carries the
 * Context fields (state/contextFields.ts). At or above it, `closed` hides it.
 */
export function sidebarVisible(mode: SidebarMode, columns: number): boolean {
  return columns >= layoutBreakpoints.sidebar && mode !== "closed"
}

/** The mode after a toggle: hide what is visible, show (`auto`) what is not; below the breakpoint nothing changes. */
export function toggledSidebar(mode: SidebarMode, columns: number): SidebarMode {
  if (columns < layoutBreakpoints.sidebar) return mode
  return sidebarVisible(mode, columns) ? "closed" : "auto"
}

/** Status line when the sidebar cannot be shown at the current width. */
export const sidebarTooNarrowNotice = `Sidebar needs ${layoutBreakpoints.sidebar}+ columns · the top status line shows its Context`

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

/**
 * How many rows `text` takes when word-wrapped (`wrapMode="word"`) at
 * `width` columns: greedy packing, breaking only between words. Used to size
 * windowed lists around fixed-height sibling lines whose text can wrap (a
 * hint or notice), so the window neither leaves a blank band nor overflows
 * past the lines below it.
 */
export function wrapLineCount(text: string, width: number): number {
  if (width <= 0) return 1
  const words = text.split(/\s+/).filter(Boolean)
  if (words.length === 0) return 1
  let lines = 1
  let column = 0
  for (const word of words) {
    if (column === 0) {
      column = word.length
      continue
    }
    if (column + 1 + word.length > width) {
      lines += 1
      column = word.length
    } else {
      column += 1 + word.length
    }
  }
  return lines
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
