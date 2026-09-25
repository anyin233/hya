/** What Esc does in the composer, in priority order. */
export type EscapeAction = "closeMenu" | "returnToParent" | "cancelTurn" | "clearInput" | "none"

export interface EscapeState {
  /** The `@file` list or the command menu is open. */
  menuOpen: boolean
  /** A turn admitted by this TUI runs (or is being admitted). */
  running: boolean
  inputEmpty: boolean
  /** A subagent's session is open read-only (its parent is known). */
  childView?: boolean
}

/**
 * Esc closes an open list first; else, in a subagent's read-only view, it
 * returns to the parent session; else it cancels the running turn; else it
 * clears the input.
 */
export function escapeAction({ menuOpen, running, inputEmpty, childView = false }: EscapeState): EscapeAction {
  if (menuOpen) return "closeMenu"
  if (childView) return "returnToParent"
  if (running) return "cancelTurn"
  return inputEmpty ? "none" : "clearInput"
}
