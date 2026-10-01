/** Single-key overrides: command assignments or disabled keys, persisted under TuiPreferences.keybindings. */
import { type KeyLike } from "./bindings"

export type CommandBindingScope = "workspace" | "conversation"
export interface CommandKeybinding {
  command: string
  scope: CommandBindingScope
}
export type CustomKeybindings = Record<string, CommandKeybinding | null>

interface Shortcut {
  label: string
  key: KeyLike
}

const names: Record<string, string> = {
  esc: "escape", enter: "return", keypadenter: "kpenter", plus: "+", space: "space", pgup: "pageup", pgdn: "pagedown", arrowleft: "left", arrowright: "right", arrowup: "up", arrowdown: "down",
}
const labels: Record<string, string> = { "+": "Plus", escape: "Esc", return: "Enter", linefeed: "Linefeed", kpenter: "KeypadEnter", pageup: "PgUp", pagedown: "PgDn", backspace: "Backspace", delete: "Delete", tab: "Tab", }

/** Parse a physical key; assignment policy is explicit, with no browser reservations. */
export function parseShortcut(text: string): Shortcut {
  const parts = text.toLowerCase().split("+")
  const raw = parts.pop() ?? ""
  const name = names[raw] ?? raw
  const modifiers = parts.map((part) => part === "cmd" || part === "command" ? "super" : part === "control" ? "ctrl" : part === "meta" || part === "option" ? "alt" : part)
  if (new Set(modifiers).size !== modifiers.length || modifiers.some((part) => !["ctrl", "alt", "shift", "super"].includes(part))) throw new Error(`Invalid shortcut: ${text}`)
  const ctrl = modifiers.includes("ctrl"), meta = modifiers.includes("alt"), shift = modifiers.includes("shift"), superKey = modifiers.includes("super")
  const named = ["left", "right", "up", "down", "home", "end", "pageup", "pagedown", "escape", "return", "linefeed", "kpenter", "tab", "backspace", "delete", "space"].includes(name)
  if (!named && !/^f([1-9]|1[0-2])$/.test(name) && !/^[!-~]$/.test(name)) throw new Error(`Unknown shortcut key: ${text}`)
  const label = `${ctrl ? "Ctrl+" : ""}${meta ? "Alt+" : ""}${shift ? "Shift+" : ""}${superKey ? "Super+" : ""}${labels[name] ?? (name[0]!.toUpperCase() + name.slice(1))}`
  return { label, key: { name, ctrl, meta, shift, super: superKey, sequence: name } }
}

function equivalentKeys(key: KeyLike): KeyLike[] {
  const aliases: Record<string, string> = { i: "tab", m: "return", j: "linefeed", h: "backspace" }
  if (key.ctrl && !key.meta && !key.shift && !key.super && aliases[key.name]) return [key, { ...key, name: aliases[key.name]!, ctrl: false, sequence: "" }]
  return [key]
}

export function sameShortcut(left: string, right: string): boolean {
  return matches(parseShortcut(left), parseShortcut(right).key)
}

function matches(shortcut: Shortcut, key: KeyLike): boolean {
  return equivalentKeys(shortcut.key).some((expected) => equivalentKeys(key).some((actual) =>
    ((expected.name.length === 1 && !expected.ctrl && !expected.meta && !expected.shift && !expected.super && actual.sequence === expected.name)
      || (actual.name === expected.name && actual.shift === expected.shift)) && actual.ctrl === expected.ctrl
    && Boolean(actual.meta || actual.option) === expected.meta && Boolean(actual.super) === Boolean(expected.super)))
}

/** A disabled physical key is consumed before default/editor/context handlers. */
export function isKeyDisabled(key: KeyLike): boolean {
  return compiled.some((entry) => entry.binding === null && matches(entry.shortcut, key))
}

/** Used by visible default catalogs, including legacy two-key prefix labels. */
export function isKeyOverridden(label: string): boolean {
  const text = label.startsWith("Ctrl+X ") ? "Ctrl+X" : label.replace(/ \(empty input\)$/, "")
  try { const shortcut = parseShortcut(text); return compiled.some((entry) => matches(entry.shortcut, shortcut.key)) } catch { return false }
}

/** Validate explicit overrides; defaults may be replaced or disabled. */
export function validateCustomKeybindings(value: unknown): CustomKeybindings {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("keybindings must be an object of shortcut labels to command bindings")
  const next: CustomKeybindings = {}
  for (const [text, binding] of Object.entries(value)) {
    const shortcut = parseShortcut(text)
    for (const previous of Object.keys(next)) {
      if (previous !== shortcut.label && matches(parseShortcut(previous), shortcut.key)) throw new Error(`${shortcut.label} shares a terminal key with ${previous}; keep one override`)
    }
    if (Object.hasOwn(next, shortcut.label)) throw new Error(`Duplicate shortcut: ${shortcut.label}`)
    if (binding === null) { next[shortcut.label] = null; continue }
    if (typeof binding !== "object" || binding === null || Array.isArray(binding)) throw new Error(`Binding for ${text} must be an object`)
    const record = binding as Record<string, unknown>
    if (typeof record.command !== "string" || !/^\/[^\s/]+(?:[ \t].*)?$/.test(record.command) || /[\r\n\x00-\x1f\x7f]/.test(record.command)) throw new Error(`Binding for ${text} must contain a single slash command`)
    if (record.scope !== "workspace" && record.scope !== "conversation") throw new Error(`Binding for ${text} needs scope workspace or conversation`)
    next[shortcut.label] = { command: record.command, scope: record.scope }
  }
  return next
}

let current: CustomKeybindings = {}
let compiled: { shortcut: Shortcut; binding: CommandKeybinding | null }[] = []

export function customKeybindings(): CustomKeybindings {
  return Object.fromEntries(Object.entries(current).map(([key, binding]) => [key, binding === null ? null : { ...binding }]))
}

/** Apply atomically after validation. The existing Composer router still owns dispatch. */
export function setCustomKeybindings(value: CustomKeybindings): void {
  const next = validateCustomKeybindings(value)
  compiled = Object.entries(next).map(([label, binding]) => ({ shortcut: parseShortcut(label), binding }))
  current = next
}

export function resolveCommandBinding(key: KeyLike): CommandKeybinding | undefined {
  const entry = compiled.find(({ shortcut, binding }) => binding !== null && matches(shortcut, key))
  return entry?.binding ? { ...entry.binding } : undefined
}
