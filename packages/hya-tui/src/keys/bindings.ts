/**
 * Key bindings (outside concealed key entry).
 *
 * `keyBindings` are the global actions. Add one by appending a row and
 * handling its action in the composer's key handler
 * (components/Composer.tsx). `composerKeyBindings` override the editing keys
 * of OpenTUI's textarea (Enter submits, Ctrl+J / Shift+Enter / Alt+Enter
 * insert a newline). Keep bindings browser-safe: never bind a core action
 * only to Ctrl/Cmd+W, T, N, L, Tab, or Ctrl+Tab (the WebUI host runs inside a
 * browser). The renderer runs with `exitOnCtrlC: false`; Ctrl+C is the
 * `quit` action here.
 */
import type { TextareaAction } from "@opentui/core"

export type KeyAction =
  | "interrupt"
  | "quit"
  | "eof"
  | "complete"
  | "refresh"
  | "toggleSidebar"
  | "toggleThinking"
  | "toggleTools"
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
    action: "interrupt",
    label: "Esc",
    description: "Close the open list, else deny/reject a shown prompt (empty input), else return from a subagent view, else cancel the running turn, else clear the input",
    matches: (key) => key.name === "escape" && !key.ctrl && !key.shift,
  },
  {
    action: "quit",
    label: "Ctrl+C",
    description: "Clear the input; press again within 2 s to quit",
    matches: (key) => key.ctrl && !key.meta && key.name === "c",
  },
  {
    action: "eof",
    label: "Ctrl+D",
    description: "Quit when the input is empty (otherwise delete the character under the cursor)",
    matches: (key, context) => key.ctrl && !key.meta && !key.shift && key.name === "d" && context.composerEmpty === true,
  },
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
    action: "toggleTools",
    label: "Ctrl+G",
    description: "Expand or collapse every tool call card",
    matches: (key) => key.ctrl && !key.meta && key.name === "g",
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

/** One textarea binding: a key (with modifiers) and the OpenTUI editing action it runs. */
export interface ComposerKeyBinding {
  name: string
  ctrl?: boolean
  shift?: boolean
  meta?: boolean
  action: TextareaAction
}

/**
 * Overrides for OpenTUI's default textarea bindings (merged by key). Enter
 * submits. Ctrl+J (a line feed) inserts a newline in every terminal and in
 * the browser; Shift+Enter only where the terminal reports it (kitty
 * keyboard protocol or modifyOtherKeys; xterm.js sends a plain CR for it, so
 * the WebUI treats it as Enter); Alt+Enter (ESC CR) works in xterm.js too.
 * Home/End move to the start/end of the current (wrapped) line and stay there.
 */
export const composerKeyBindings: readonly ComposerKeyBinding[] = [
  { name: "return", action: "submit" },
  { name: "kpenter", action: "submit" },
  { name: "linefeed", action: "newline" },
  { name: "j", ctrl: true, action: "newline" },
  { name: "return", shift: true, action: "newline" },
  { name: "return", meta: true, action: "newline" },
  { name: "kpenter", meta: true, action: "newline" },
  { name: "home", action: "visual-line-home" },
  { name: "end", action: "visual-line-end" },
]
