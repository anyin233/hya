/**
 * The reusable modal picker's pure state (components/Picker.tsx renders it;
 * docs/tui.md "Code layout" documents the API). A picker is a titled,
 * filterable list of rows `{id, label, detail?, tag?, current?}`: typing
 * filters (every word must match the label, id, tag, or detail), Up/Down
 * (and Shift+Tab/Tab) move the highlight with wrap-around, Enter selects the
 * highlighted row, Esc closes. `pickerKey` maps one key to an outcome; the
 * caller (the controller) stores the new state, runs the selection, or closes.
 */
import type { KeyLike } from "../keys/bindings"

export interface PickerRow {
  /** Value handed to the selection callback (a mode id, a model id, a session id, …). */
  id: string
  /** Main text of the row. */
  label: string
  /** Muted text after the tag (a description). */
  detail?: string
  /** Short bracketed source or kind (`builtin`, a bundle id, a provider). */
  tag?: string
  /** The value in effect now: marked `●` and highlighted when the picker opens. */
  current?: boolean
}

export interface PickerState {
  title: string
  rows: PickerRow[]
  /** Filter text typed so far. */
  query: string
  /** Highlighted row within the filtered rows. */
  index: number
  /** Bottom hint row; a default key hint when omitted. */
  hint?: string
}

export type PickerOutcome =
  | { type: "none" }
  | { type: "update"; state: PickerState }
  | { type: "select"; row: PickerRow }
  | { type: "close" }

export const defaultPickerHint = "↑↓ select · Enter chooses · Esc closes · type to filter"

/** Most rows a picker shows at once; the window scrolls with the highlight. */
export const pickerMaxRows = 10

/** Index of the current row in `rows`, else 0. */
function currentIndex(rows: PickerRow[]): number {
  return Math.max(0, rows.findIndex((row) => row.current))
}

export function createPicker(options: { title: string; rows: PickerRow[]; hint?: string }): PickerState {
  return { title: options.title, rows: options.rows, query: "", index: currentIndex(options.rows), ...(options.hint ? { hint: options.hint } : {}) }
}

/** Rows matching every whitespace-separated word of `query` (case-insensitive substring of label, id, tag, or detail). */
export function pickerMatches(rows: PickerRow[], query: string): PickerRow[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean)
  if (!words.length) return rows
  return rows.filter((row) => {
    const haystack = `${row.label}\n${row.id}\n${row.tag ?? ""}\n${row.detail ?? ""}`.toLowerCase()
    return words.every((word) => haystack.includes(word))
  })
}

/** The rows shown now (the filtered list). */
export function pickerRows(state: PickerState): PickerRow[] {
  return pickerMatches(state.rows, state.query)
}

function withQuery(state: PickerState, query: string): PickerState {
  // An empty filter shows the whole list again, highlighting the current row.
  return { ...state, query, index: query ? 0 : currentIndex(state.rows) }
}

export function pickerKey(state: PickerState, key: KeyLike): PickerOutcome {
  const rows = pickerRows(state)
  const move = (step: number): PickerOutcome => rows.length
    ? { type: "update", state: { ...state, index: (state.index + step + rows.length) % rows.length } }
    : { type: "none" }
  if (key.name === "escape") return { type: "close" }
  if (key.name === "return" || key.name === "kpenter") {
    const row = rows[state.index]
    return row ? { type: "select", row } : { type: "none" }
  }
  if (key.ctrl || key.meta) {
    if (key.ctrl && !key.meta && key.name === "u") return { type: "update", state: withQuery(state, "") }
    return { type: "none" }
  }
  if (key.name === "up" || (key.name === "tab" && key.shift)) return move(-1)
  if (key.name === "down" || key.name === "tab") return move(1)
  if (key.name === "backspace") return state.query ? { type: "update", state: withQuery(state, state.query.slice(0, -1)) } : { type: "none" }
  const text = key.sequence
  if (text.length === 1 && text >= " " && text !== "\x7f") return { type: "update", state: withQuery(state, state.query + text) }
  return { type: "none" }
}

/** The `[start, end)` slice of `count` rows to show in `height` rows so `index` stays visible. */
export function pickerWindow(count: number, index: number, height: number): { start: number; end: number } {
  if (count <= height) return { start: 0, end: count }
  const start = Math.min(Math.max(0, index - height + 1), count - height)
  return { start, end: start + height }
}

/** What a caller hands to `openPicker`: the title, rows, optional hint, and what choosing a row does. */
export interface PickerSpec {
  title: string
  rows: PickerRow[]
  hint?: string
  /** Runs after the picker closed (focus is back on the composer). */
  onSelect(row: PickerRow): void | Promise<void>
}

/** An open picker: its list state plus what choosing a row does. */
export interface ActivePicker extends PickerState {
  onSelect(row: PickerRow): void | Promise<void>
}
