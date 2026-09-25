/**
 * Global key bindings (outside concealed key entry).
 *
 * Add a binding by appending a row and handling its action in the composer's
 * key handler (components/Composer.tsx). Keep bindings browser-safe: never
 * bind a core action only to Ctrl/Cmd+W, T, N, L, Tab, or Ctrl+Tab (the WebUI
 * host runs inside a browser). Ctrl+C quit is handled by the renderer itself.
 */

export type KeyAction = "complete" | "refresh"

/** The subset of OpenTUI's KeyEvent a binding looks at. */
export interface KeyLike {
  name: string
  ctrl: boolean
  meta: boolean
  shift: boolean
  sequence: string
}

export interface KeyBinding {
  action: KeyAction
  /** Human label, e.g. `Ctrl+R`. */
  label: string
  description: string
  matches(key: KeyLike): boolean
}

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
]

export function resolveBinding(key: KeyLike, bindings: readonly KeyBinding[] = keyBindings): KeyAction | undefined {
  return bindings.find((binding) => binding.matches(key))?.action
}
