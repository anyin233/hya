/**
 * Key and command help (G29): the `/help` / `?` overlay and the key help
 * view are generated from the binding tables (`keyBindings`,
 * `composerKeyBindings`, the `/sessions` picker actions) and the merged
 * command list, so they cannot drift from what the keys do. Prompt and
 * picker keys, which live in state machines (state/prompts.ts `promptKey`,
 * state/picker.ts `pickerKey`) rather than tables, are listed here next to
 * those machines' precedence rules; keep them in step.
 */
import type { TextareaAction } from "@opentui/core"
import { composerKeyBindings, keyBindings, type ComposerKeyBinding, type KeyAction } from "../keys/bindings"
import type { PickerRow } from "../state/picker"
import type { CommandEntry } from "./menu"
import { sessionPickerActions } from "./native"

export const helpGroups = ["Composer", "Transcript", "Turns", "Prompts", "Modes", "Pickers", "Views", "App", "Commands"] as const
export type HelpGroup = (typeof helpGroups)[number]

export interface HelpRow {
  group: HelpGroup
  /** Key label(s), `A / B` for alternatives, or the command with its argument hint. */
  keys: string
  description: string
  /** Commands only: `local` (this TUI), `server` (backend catalog), or `skill`. */
  source?: "local" | "server" | "skill"
}

/** The help group of every global key action (a new action fails to compile until it has one). */
const actionGroups: Record<KeyAction, HelpGroup> = {
  interrupt: "Turns",
  quit: "App",
  eof: "App",
  complete: "Composer",
  cycleMode: "Modes",
  refresh: "Views",
  toggleSidebar: "Views",
  toggleThinking: "Views",
  toggleTools: "Views",
  pageUp: "Transcript",
  pageDown: "Transcript",
  scrollTop: "Transcript",
  scrollBottom: "Transcript",
  help: "Views",
}

/** Longest joined key label (the picker's label column is 28 wide). */
const helpLabelWidth = 26

const keyNames: Record<string, string> = { return: "Enter", kpenter: "Keypad Enter", linefeed: "Ctrl+J", home: "Home", end: "End" }

/** `Ctrl+J`, `Shift+Enter`, `Alt+Enter`, … for one textarea binding. */
export function composerKeyLabel(binding: ComposerKeyBinding): string {
  const name = keyNames[binding.name] ?? (binding.name.length === 1 ? binding.name.toUpperCase() : binding.name)
  if (binding.name === "linefeed") return name
  return `${binding.ctrl ? "Ctrl+" : ""}${binding.meta ? "Alt+" : ""}${binding.shift ? "Shift+" : ""}${name}`
}

const editingText: Partial<Record<TextareaAction, string>> = {
  submit: "Send the input (a prompt, /command, or !command)",
  newline: "Insert a newline",
  "visual-line-home": "Cursor to the start of the line (Home also scrolls to the top when the input is empty)",
  "visual-line-end": "Cursor to the end of the line (End also scrolls to the bottom when the input is empty)",
}

/**
 * xterm.js (the WebUI) and terminals without the kitty keyboard protocol or
 * modifyOtherKeys send a plain CR for Shift+Enter, which then sends.
 */
const terminalOnly = (binding: ComposerKeyBinding): boolean => binding.name === "return" && binding.shift === true

function composerRows(): HelpRow[] {
  const groups = new Map<string, { labels: string[]; description: string }>()
  for (const binding of composerKeyBindings) {
    const special = terminalOnly(binding)
    const key = `${binding.action}${special ? ":terminal" : ""}`
    const base = editingText[binding.action] ?? binding.action
    const description = special ? `${base} (terminal only: xterm.js and the WebUI send a plain Enter for it)` : base
    const entry = groups.get(key) ?? { labels: [], description }
    const label = composerKeyLabel(binding)
    if (!entry.labels.includes(label)) entry.labels.push(label)
    groups.set(key, entry)
  }
  // Alternatives share a row while the label stays short enough for the picker's label column.
  const rows: HelpRow[] = []
  for (const entry of groups.values()) {
    let keys = ""
    for (const label of entry.labels) {
      if (keys && `${keys} / ${label}`.length > helpLabelWidth) {
        rows.push({ group: "Composer", keys, description: entry.description })
        keys = label
      } else keys = keys ? `${keys} / ${label}` : label
    }
    if (keys) rows.push({ group: "Composer", keys, description: entry.description })
  }
  return [
    ...rows,
    { group: "Composer", keys: "Up / Down", description: "On the first / last line: the previous / next submitted input (in an open list: move)" },
    { group: "Composer", keys: "/", description: "At the start of the input: open the command menu (fuzzy filter; Enter runs or completes)" },
    { group: "Composer", keys: "!<command>", description: "Run a shell command in the session (ShellTurn)" },
    { group: "Composer", keys: "@<text>", description: "Pick a file path to reference (Up/Down, Tab/Enter insert, Esc closes)" },
  ]
}

/** Keys of the permission/question prompt (state/prompts.ts `promptKey`): they win over the bindings while a prompt shows. */
const promptRows: HelpRow[] = [
  { group: "Prompts", keys: "1 / 2 / 3", description: "Permission prompt, input empty: allow once / always allow / deny" },
  { group: "Prompts", keys: "1-9", description: "Question prompt, input empty: pick that option" },
  { group: "Prompts", keys: "Up / Down, Enter", description: "Move the highlighted option, choose it" },
  { group: "Prompts", keys: "text + Enter", description: "Question prompt: answer with the typed text" },
  { group: "Prompts", keys: "Esc", description: "Input empty: deny the permission / reject the question" },
]

/** Keys of the modal picker (state/picker.ts `pickerKey`): it takes every key but Ctrl+C while open. */
function pickerRows(): HelpRow[] {
  return [
    { group: "Pickers", keys: "Up / Down, Shift+Tab / Tab", description: "Move the highlight (wraps around)" },
    { group: "Pickers", keys: "Enter, click", description: "Choose the highlighted row" },
    { group: "Pickers", keys: "type, Backspace, Ctrl+U", description: "Filter the rows, widen, clear the filter" },
    { group: "Pickers", keys: "Esc", description: "Close the picker (Ctrl+C closes it too and keeps its quit meaning)" },
    ...sessionPickerActions.map((action) => ({ group: "Pickers" as const, keys: action.label.split(" ")[0]!, description: `/sessions: ${action.label}${action.prompt === "confirm" ? " (asks to confirm)" : " (edit inline, Enter saves)"}` })),
  ]
}

/** Mouse actions (no binding table; components/Transcript.tsx and MessageView.tsx). */
const mouseRows: HelpRow[] = [
  { group: "Transcript", keys: "Mouse wheel", description: "Scroll the transcript" },
  { group: "Transcript", keys: "Click Thinking / a tool card", description: "Expand or collapse that block; a task card opens the subagent's session" },
]

const sources: Record<CommandEntry["source"], NonNullable<HelpRow["source"]>> = { local: "local", command: "server", skill: "skill" }

/** Every key (grouped, in `helpGroups` order) and every command of `commands` (the merged `/` menu list). */
export function helpRows(commands: readonly CommandEntry[]): HelpRow[] {
  const bindingRows: HelpRow[] = keyBindings.map((binding) => ({ group: actionGroups[binding.action], keys: binding.label, description: binding.description }))
  const commandRows: HelpRow[] = commands.map((entry) => ({
    group: "Commands",
    keys: `${entry.name}${entry.argumentHint ? ` ${entry.argumentHint}` : ""}`,
    description: entry.description,
    source: sources[entry.source],
  }))
  const rows = [...composerRows(), ...bindingRows, ...mouseRows, ...promptRows, ...pickerRows(), ...commandRows]
  // Stable sort: table order within a group.
  return rows.map((row, index) => ({ row, index }))
    .sort((a, b) => helpGroups.indexOf(a.row.group) - helpGroups.indexOf(b.row.group) || a.index - b.index)
    .map(({ row }) => row)
}

/** Rows for the help overlay (the modal picker): keys tagged by group, commands by source. */
export function helpPickerRows(commands: readonly CommandEntry[]): PickerRow[] {
  return helpRows(commands).map((row) => ({
    id: `${row.group}:${row.keys}`,
    label: row.keys,
    tag: row.source ?? row.group.toLowerCase(),
    detail: row.description,
  }))
}

export const helpPickerHint = "↑↓ scroll · type to filter (a key, a command, or a group) · Esc closes"

/** Plain-text key help (the `help` view shown when the TUI cannot reach its backend). */
export function keyHelpText(): string {
  const lines: string[] = []
  let group: HelpGroup | undefined
  for (const row of helpRows([])) {
    if (row.group !== group) {
      group = row.group
      lines.push(lines.length ? "" : "", group)
    }
    lines.push(`  ${row.keys.padEnd(26)} ${row.description}`)
  }
  lines.push("", "Commands", "  Type / for the command menu once connected; /help lists every command.")
  return lines.join("\n").trimStart()
}
