/**
 * Global key bindings (outside concealed key entry).
 *
 * Add a binding by appending a row and handling its action in the composer's
 * key handler (components/Composer.tsx). Keep bindings browser-safe: never
 * bind a core action only to Ctrl/Cmd+W, T, N, L, Tab, or Ctrl+Tab (the WebUI
 * host runs inside a browser). Ctrl+C quit is handled by the renderer itself.
 */

export type KeyAction =
  | "complete"
  | "refresh"
  | "toggleSidebar"
  | "toggleThinking"
  | "pageUp"
  | "pageDown"
  | "scrollTop"
  | "scrollBottom"

/** The subset of OpenTUI's KeyEvent a binding looks at. */
export interface KeyLike {
  name: string
  ctrl: boolean
  meta: boolean
  shift: boolean
  sequence: string
}

/** Input state a binding may depend on. */
export interface KeyContext {
  /** The composer holds no text (plain Home/End then scroll the transcript). */
  composerEmpty?: boolean
}

export interface KeyBinding {
  action: KeyAction
  /** Human label, e.g. `Ctrl+R`. */
  label: string
  description: string
  matches(key: KeyLike, context: KeyContext): boolean
}

const plain = (key: KeyLike): boolean => !key.ctrl && !key.meta && !key.shift

export const keyBindings: readonly KeyBinding[] = [
  {
    action: "complete",
    label: "Tab",
    description: "Complete the /command or argument; repeat to cycle",
    matches: (key) => key.name === "tab" || key.sequence === "\t",
  },
  {
    action: "refresh",
    label: "Ctrl+R",
    description: "Refresh sessions, catalogs, and the transcript",
    matches: (key) => key.ctrl && key.name === "r",
  },
  {
    action: "toggleSidebar",
    label: "Ctrl+B",
    description: "Show or hide the sidebar (sessions, todos, context)",
    matches: (key) => key.ctrl && !key.meta && key.name === "b",
  },
  {
    action: "toggleThinking",
    label: "Ctrl+O",
    description: "Expand or collapse every reasoning (Thinking) block",
    matches: (key) => key.ctrl && !key.meta && key.name === "o",
  },
  {
    action: "pageUp",
    label: "PgUp",
    description: "Scroll the transcript up one page",
    matches: (key) => key.name === "pageup" && !key.ctrl,
  },
  {
    action: "pageDown",
    label: "PgDn",
    description: "Scroll the transcript down one page",
    matches: (key) => key.name === "pagedown" && !key.ctrl,
  },
  {
    action: "scrollTop",
    label: "Ctrl+Home",
    description: "Jump to the top of the transcript (plain Home when the composer is empty)",
    matches: (key, context) => key.name === "home" && (key.ctrl || (plain(key) && context.composerEmpty === true)),
  },
  {
    action: "scrollBottom",
    label: "Ctrl+End",
    description: "Jump to the newest line and follow it (plain End when the composer is empty)",
    matches: (key, context) => key.name === "end" && (key.ctrl || (plain(key) && context.composerEmpty === true)),
  },
]

export function resolveBinding(key: KeyLike, context: KeyContext = {}, bindings: readonly KeyBinding[] = keyBindings): KeyAction | undefined {
  return bindings.find((binding) => binding.matches(key, context))?.action
}
