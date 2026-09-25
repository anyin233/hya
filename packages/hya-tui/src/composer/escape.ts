/** What Esc does in the composer, in priority order. */
export type EscapeAction = "closeMenu" | "cancelTurn" | "clearInput" | "none"

export interface EscapeState {
  /** The `@file` suggestion list is open. */
  menuOpen: boolean
  /** A turn admitted by this TUI runs (or is being admitted). */
  running: boolean
  inputEmpty: boolean
}

/** Esc closes the `@file` list first, else cancels the running turn, else clears the input. */
export function escapeAction({ menuOpen, running, inputEmpty }: EscapeState): EscapeAction {
  if (menuOpen) return "closeMenu"
  if (running) return "cancelTurn"
  return inputEmpty ? "none" : "clearInput"
}
