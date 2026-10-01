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
  | "openCommands"
  | "focusPaneLeft"
  | "focusPaneRight"
  | "focusPaneUp"
  | "focusPaneDown"
  | "cycleMode"
  | "refresh"
  | "toggleSidebar"
  | "toggleProjectsSidebar"
  | "toggleThinking"
  | "toggleTools"
  | "pageUp"
  | "pageDown"
  | "scrollTop"
  | "scrollBottom"
  | "help"
  | "chord"
  | "externalEditor"
  | "undo"
  | "redo"
  | "fork"
  | "reviewPending"

/** The subset of OpenTUI's KeyEvent a binding looks at. */
export interface KeyLike {
  name: string
  ctrl: boolean
  meta: boolean
  /** Kitty's Option modifier; traditional terminals report Alt as `meta`. */
  option?: boolean
  shift: boolean
  sequence: string
}

/** Input state a binding may depend on. */
export interface KeyContext {
  /** The composer holds no text (plain Home/End then scroll the transcript). */
  composerEmpty?: boolean
  /** The first key of a two-key chord was pressed (`Ctrl+X`); the next key completes or drops it. */
  chord?: "ctrl+x"
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
  ...(["left", "right", "up", "down"] as const).map((direction) => ({
    action: `focusPane${direction[0]!.toUpperCase()}${direction.slice(1)}` as KeyAction,
    label: `Alt+${direction[0]!.toUpperCase()}${direction.slice(1)}`,
    description: `Focus the ${direction} tiled pane`,
    matches: (key: KeyLike) => (key.meta || key.option === true) && !key.ctrl && !key.shift && key.name === direction,
  })),
  // The second key of a Ctrl+X chord comes first: while the chord is armed it wins over every other binding.
  {
    action: "openCommands",
    label: "Ctrl+X /",
    description: "Focus the command pane without changing the message draft",
    matches: (key, context) => context.chord === "ctrl+x" && !key.ctrl && !key.meta && key.sequence === "/",
  },
  {
    action: "chord",
    label: "Ctrl+X",
    description: "Open commands with Ctrl+X / while drafting; any other next key cancels the prefix",
    matches: (key, context) => key.ctrl && !key.meta && !key.shift && key.name === "x" && context.chord === undefined,
  },
  {
    action: "interrupt",
    label: "Esc",
    description: "Close the open list, else deny/reject a shown prompt (empty input), else return from a subagent view, else cancel the running turn, else clear the input",
    matches: (key) => key.name === "escape" && !key.ctrl && !key.shift,
  },
  {
    action: "quit",
    label: "Ctrl+C",
    description: "Clear the input; press again within 2 s to quit and archive the session (like /exit; /resume brings it back)",
    matches: (key) => key.ctrl && !key.meta && key.name === "c",
  },
  {
    action: "eof",
    label: "Ctrl+D",
    description: "On an empty input: quit and leave the session running on the backend (like /to-background); in a WebUI tab, close the tab instead. Otherwise delete the character under the cursor",
    matches: (key, context) => key.ctrl && !key.meta && !key.shift && key.name === "d" && context.composerEmpty === true,
  },
  {
    action: "complete",
    label: "Tab",
    description: "Complete the selected file reference in a prompt",
    matches: (key) => plain(key) && (key.name === "tab" || key.sequence === "\t"),
  },
  {
    action: "openCommands",
    label: "/",
    description: "Focus the command pane when the message editor is empty; use Ctrl+X / while writing a message",
    matches: (key, context) => !key.ctrl && !key.meta && key.sequence === "/" && context.composerEmpty === true,
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
    label: "Home (empty input)",
    description: "Jump to the top of the transcript when the composer is empty",
    matches: (key, context) => key.name === "home" && plain(key) && context.composerEmpty === true,
  },
  {
    action: "scrollBottom",
    label: "End (empty input)",
    description: "Jump to the newest line and follow it when the composer is empty",
    matches: (key, context) => key.name === "end" && plain(key) && context.composerEmpty === true,
  },
  {
    action: "help",
    label: "?",
    description: "Show every key and command (when the input is empty; otherwise types ?)",
    matches: (key, context) => key.sequence === "?" && !key.ctrl && !key.meta && context.composerEmpty === true,
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
 * submits. Ctrl+J (a line feed), Shift+Enter, and Alt+Enter insert a newline.
 * The WebUI translates Shift+Enter to a line feed because xterm.js otherwise
 * reports it as a plain Enter. Home/End move to the start/end of the current
 * (wrapped) line and stay there.
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
