/** Single-key custom shortcuts, persisted under TuiPreferences.keybindings. */
import { keyBindings, type KeyLike } from "./bindings"

export type CommandBindingScope = "workspace" | "conversation"
export interface CommandKeybinding {
  command: string
  scope: CommandBindingScope
}
export type CustomKeybindings = Record<string, CommandKeybinding>

interface Shortcut {
  label: string
  key: KeyLike
}

const names: Record<string, string> = {
  pgup: "pageup", pgdn: "pagedown", arrowleft: "left", arrowright: "right", arrowup: "up", arrowdown: "down",
}
const labels: Record<string, string> = { pageup: "PgUp", pagedown: "PgDn" }

/** Parse a modified letter/navigation key or F1–F12. Plain typing/editing keys are not assignable. */
export function parseShortcut(text: string): Shortcut {
  const parts = text.toLowerCase().split("+")
  const raw = parts.pop() ?? ""
  const name = names[raw] ?? raw
  const modifiers = parts.map((part) => part === "control" ? "ctrl" : part === "meta" || part === "option" ? "alt" : part)
  if (new Set(modifiers).size !== modifiers.length || modifiers.some((part) => !["ctrl", "alt", "shift"].includes(part))) throw new Error(`Invalid shortcut: ${text}`)
  const ctrl = modifiers.includes("ctrl"), meta = modifiers.includes("alt"), shift = modifiers.includes("shift")
  const letter = /^[a-z0-9]$/.test(name)
  const navigation = ["left", "right", "up", "down", "home", "end", "pageup", "pagedown"].includes(name)
  const fn = /^f([1-9]|1[0-2])$/.test(name)
  if (!fn && !((letter || navigation) && (ctrl || meta))) throw new Error("Use a modified letter/navigation key or F1–F12 (for example Alt+G or F6)")
  if (ctrl && shift && letter) throw new Error("Ctrl+Shift letters are not distinguished reliably by terminals; use Alt or a function key")
  if (ctrl && !meta && ["w", "t", "n", "l", "i", "m", "j", "h"].includes(name)) throw new Error(`Reserved browser or text-editing shortcut: ${text}`)
  const label = `${ctrl ? "Ctrl+" : ""}${meta ? "Alt+" : ""}${shift ? "Shift+" : ""}${labels[name] ?? (name[0]!.toUpperCase() + name.slice(1))}`
  return { label, key: { name, ctrl, meta, shift, sequence: name } }
}

function matches(shortcut: Shortcut, key: KeyLike): boolean {
  return key.name === shortcut.key.name && key.ctrl === shortcut.key.ctrl
    && Boolean(key.meta || key.option) === shortcut.key.meta && key.shift === shortcut.key.shift
}

/** Validate all assignments together, including collisions with contextual built-ins. */
export function validateCustomKeybindings(value: unknown): CustomKeybindings {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("keybindings must be an object of shortcut labels to command bindings")
  const next: CustomKeybindings = {}
  for (const [text, binding] of Object.entries(value)) {
    const shortcut = parseShortcut(text)
    if (typeof binding !== "object" || binding === null || Array.isArray(binding)) throw new Error(`Binding for ${text} must be an object`)
    const record = binding as Record<string, unknown>
    if (typeof record.command !== "string" || !/^\/[^\s/]+(?:[ \t].*)?$/.test(record.command) || /[\r\n\x00-\x1f\x7f]/.test(record.command)) throw new Error(`Binding for ${text} must contain a single slash command`)
    if (record.scope !== "workspace" && record.scope !== "conversation") throw new Error(`Binding for ${text} needs scope workspace or conversation`)
    const conflict = keyBindings.find((binding) => [
      {}, { composerEmpty: true }, { composerEmpty: false }, { chord: "ctrl+x" as const },
    ].some((context) => binding.matches(shortcut.key, context)))
    if (conflict) throw new Error(`${shortcut.label} is already bound to ${conflict.action}`)
    if (next[shortcut.label]) throw new Error(`Duplicate shortcut: ${shortcut.label}`)
    next[shortcut.label] = { command: record.command, scope: record.scope }
  }
  return next
}

let current: CustomKeybindings = {}
let compiled: { shortcut: Shortcut; binding: CommandKeybinding }[] = []

export function customKeybindings(): CustomKeybindings {
  return Object.fromEntries(Object.entries(current).map(([key, binding]) => [key, { ...binding }]))
}

/** Apply atomically after validation. The existing Composer router still owns dispatch. */
export function setCustomKeybindings(value: CustomKeybindings): void {
  const next = validateCustomKeybindings(value)
  compiled = Object.entries(next).map(([label, binding]) => ({ shortcut: parseShortcut(label), binding }))
  current = next
}

export function resolveCommandBinding(key: KeyLike): CommandKeybinding | undefined {
  const entry = compiled.find(({ shortcut }) => matches(shortcut, key))
  return entry ? { ...entry.binding } : undefined
}
