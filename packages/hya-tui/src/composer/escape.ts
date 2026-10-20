/** What Esc does in the composer, in priority order. */
export type EscapeAction = "closeMenu" | "declinePrompt" | "returnToParent" | "cancelTurn" | "clearInput" | "none"

export interface EscapeState {
  /** The `@file` list is open (the command pane handles its own Esc). */
  menuOpen: boolean
  /** A turn admitted by this TUI runs (or is being admitted). */
  running: boolean
  /** A subagent of the open session is working (its own turn ended or not). */
  subagentsWorking?: boolean
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
 * clears the input; else, with the parent's turn over, it stops its working
 * subagents.
 */
export function escapeAction({ menuOpen, running, inputEmpty, childView = false, prompt = false, subagentsWorking = false }: EscapeState): EscapeAction {
  if (menuOpen) return "closeMenu"
  if (prompt && inputEmpty) return "declinePrompt"
  if (childView) return "returnToParent"
  if (running) return "cancelTurn"
  if (!inputEmpty) return "clearInput"
  return subagentsWorking ? "cancelTurn" : "none"
}
