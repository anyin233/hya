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
export const layoutBreakpoints = { sidebar: 110 } as const

const sidebarMax = 32
const sidebarMin = 20

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
