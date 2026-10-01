/** Discoverable inherited/editor/context bindings, using the existing key tables. */
import { defaultTextareaKeyBindings } from "@opentui/core"
import { helpRows } from "../commands/help"
import { composerKeyBindings } from "./bindings"
import { isKeyOverridden, parseShortcut } from "./custom"
import type { PickerRow } from "../state/picker"

let cachedRows: PickerRow[] | undefined

export function inheritedBindingRows(): PickerRow[] {
  if (cachedRows) return cachedRows.map((row) => ({ ...row }))
  const rows: PickerRow[] = []
  const editing = [...defaultTextareaKeyBindings, ...composerKeyBindings]
  const byKey = new Map<string, typeof editing[number]>()
  for (const entry of editing) {
    const label = `${entry.ctrl ? "Ctrl+" : ""}${entry.meta ? "Alt+" : ""}${entry.shift ? "Shift+" : ""}${"super" in entry && entry.super ? "Super+" : ""}${entry.name}`
    try { byKey.set(parseShortcut(label).label, entry) } catch { /* Unknown terminal spelling is not a shortcut. */ }
  }
  for (const [shortcut, entry] of byKey) rows.push({ id: `editor:${shortcut}`, shortcut, label: entry.action, tag: "conversation", detail: `Message editor: ${entry.action}. Inherited from OpenTUI or the composer override. App actions take precedence; custom commands override this behavior.` })
  const contexts = helpRows([]).filter((row) => !["App", "Views", "Commands", "Transcript", "Composer", "Turns"].includes(row.group))
  contexts.push(
    { group: "Modes", keys: "Enter / Esc / Shift+Tab", description: "Yolo confirmation: confirm, cancel, or skip the target mode" },
    { group: "Views", keys: "n", description: "Project view: create a Project" },
    { group: "Views", keys: "t", description: "Project view: temporary session" },
    { group: "Views", keys: "r", description: "Project view: rename" },
    { group: "Views", keys: "e", description: "Project view: edit roots" },
    { group: "Views", keys: "d", description: "Project view: delete" },
  )
  for (const [index, row] of contexts.entries()) {
    for (const part of row.keys.split(/[,/]/).flatMap((part) => part.trim().split(/\s+/).filter((token) => token !== "motion"))) {
      const token = part.trim()
      const key = /^(.)\1$/.test(token) ? token[0]! : token
      try {
        const shortcut = parseShortcut(/^[A-Z]$/.test(key) ? `Shift+${key}` : key).label
        rows.push({ id: `context:${row.group}:${index}:${shortcut}`, shortcut, label: `${row.group}: ${token !== key ? `${token} prefix · ` : ""}${row.description}`, tag: row.group === "Vim" || row.group === "Prompts" || row.group === "Modes" ? "conversation" : "workspace", detail: `${row.group} only: ${row.description}. This view owns input before custom command bindings; unset disables the physical key outside the command input.` })
      } catch { /* Prose, mouse actions and Vim sequences are not single shortcuts. */ }
    }
  }
  for (const shortcut of ["Esc", "Enter", "Tab", "Up", "Down", "Shift+Up", "Shift+Down", "Backspace", "Ctrl+C"]) {
    rows.push({ id: `context:Command:${shortcut}`, shortcut, label: "Command input", tag: "workspace", detail: "Command input owns editing, completion, history, submit and close keys even when a workspace shortcut is disabled. This keeps settings repair accessible." })
  }
  cachedRows = rows
  return rows.map((row) => ({ ...row }))
}

export function activeInheritedBindingRows(): PickerRow[] {
  return inheritedBindingRows().filter((row) => row.id.startsWith("context:Command:") || !isKeyOverridden(row.shortcut!))
}
