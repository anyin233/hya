/** What Esc does in the composer, in priority order. */
export type EscapeAction = "closeMenu" | "declinePrompt" | "returnToParent" | "cancelTurn" | "clearInput" | "none"

export interface EscapeState {
  /** The `@file` list or the command menu is open. */
  menuOpen: boolean
  /** A turn admitted by this TUI runs (or is being admitted). */
  running: boolean
  inputEmpty: boolean
  /** A subagent's session is open read-only (its parent is known). */
  childView?: boolean
  /** A permission or question prompt is shown (state/prompts.ts). */
  prompt?: boolean
}

/**
 * Esc closes an open list first; else, with a prompt shown and an empty
 * input, it declines the prompt (denies the permission / rejects the
 * question — never approves); else, in a subagent's read-only view, it
 * returns to the parent session; else it cancels the running turn; else it
 * clears the input.
 */
export function escapeAction({ menuOpen, running, inputEmpty, childView = false, prompt = false }: EscapeState): EscapeAction {
  if (menuOpen) return "closeMenu"
  if (prompt && inputEmpty) return "declinePrompt"
  if (childView) return "returnToParent"
  if (running) return "cancelTurn"
  return inputEmpty ? "none" : "clearInput"
}
