/**
 * The reusable modal picker's pure state (components/Picker.tsx renders it;
 * docs/tui.md "Code layout" documents the API). A picker is a titled,
 * filterable list of rows `{id, label, detail?, tag?, current?}`: typing
 * filters (every word must match the label, id, tag, or detail), Up/Down
 * (and Shift+Tab/Tab) move the highlight with wrap-around, Enter selects the
 * highlighted row, Esc closes. `pickerKey` maps one key to an outcome; the
 * caller (the controller) stores the new state, runs the selection, or closes.
 *
 * Row actions (S9, C13): a picker may declare `actions`, each bound to one
 * key (not a plain character, so it never collides with the filter) not
 * reserved by the browser host. Triggering one switches the picker into
 * `"rename"` (edit the row's label inline) or `"confirm"` (a one-line yes/no)
 * mode; Enter there commits `{type: "commit", id, row, value?}` — the
 * controller closes the picker and runs the spec's `onAction`, the same way
 * `onSelect` runs after a plain `select`. Esc backs out to the list without
 * closing the picker.
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

/** A row action bound to one key (S9, C13: `/sessions` rename/delete). */
export interface PickerAction {
  /** Outcome id (`onAction`'s first argument, e.g. `"rename"`, `"delete"`). */
  id: string
  /** OpenTUI key name (`"f2"`, `"d"`, …), never a plain printable character. */
  key: string
  ctrl?: boolean
  /** Hint text, e.g. `"F2 rename"`. */
  label: string
  /** `"value"` edits the row's label inline before committing (rename); `"confirm"` shows a yes/no line (delete). */
  prompt: "value" | "confirm"
  /** Confirmation text template for `prompt: "confirm"`; `{label}` is replaced by the row's label. */
  confirmText?: string
}

export type PickerMode = "list" | "rename" | "confirm"

export interface PickerState {
  title: string
  rows: PickerRow[]
  /** Filter text typed so far. */
  query: string
  /** Highlighted row within the filtered rows. */
  index: number
  /** Bottom hint row; a default key hint when omitted. */
  hint?: string
  /** Row actions available on the highlighted row (F2 rename, Ctrl+D delete, …). */
  actions?: readonly PickerAction[]
  /** Most rows shown at once (default `pickerMaxRows`); the terminal height bounds it too (the help overlay asks for more). */
  maxRows?: number
  /** Show the highlighted row's whole `detail`, wrapped, under the list (rows clip it to one line). */
  detailPane?: boolean
  /** `"list"` (default) browses; `"rename"`/`"confirm"` are a row action in progress. */
  mode?: PickerMode
  /** `id` of the row a `"rename"`/`"confirm"` mode targets. */
  actionRow?: string
  /** `id` of the `PickerAction` a `"rename"`/`"confirm"` mode is running. */
  actionId?: string
  /** Text edited while `mode` is `"rename"`. */
  editValue?: string
  /** Line shown while `mode` is `"confirm"`. */
  confirmText?: string
}

export type PickerOutcome =
  | { type: "none" }
  | { type: "update"; state: PickerState }
  | { type: "select"; row: PickerRow }
  | { type: "commit"; id: string; row: PickerRow; value?: string }
  | { type: "close" }

export const defaultPickerHint = "↑↓ select · Enter chooses · Esc closes · type to filter"

/** Most rows a picker shows at once; the window scrolls with the highlight. */
export const pickerMaxRows = 10

/** Index of the current row in `rows`, else 0. */
function currentIndex(rows: PickerRow[]): number {
  return Math.max(0, rows.findIndex((row) => row.current))
}

export function createPicker(options: { title: string; rows: PickerRow[]; hint?: string; actions?: readonly PickerAction[]; maxRows?: number; detailPane?: boolean }): PickerState {
  return {
    title: options.title,
    rows: options.rows,
    query: "",
    index: currentIndex(options.rows),
    mode: "list",
    ...(options.hint ? { hint: options.hint } : {}),
    ...(options.actions ? { actions: options.actions } : {}),
    ...(options.maxRows ? { maxRows: options.maxRows } : {}),
    ...(options.detailPane ? { detailPane: true } : {}),
  }
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

/** Back out of a `"rename"`/`"confirm"` mode to the plain list, keeping the filter and highlight. */
function toList(state: PickerState): PickerState {
  return { ...state, mode: "list", actionRow: undefined, actionId: undefined, editValue: undefined, confirmText: undefined }
}

function renameKey(state: PickerState, key: KeyLike): PickerOutcome {
  if (key.name === "escape") return { type: "update", state: toList(state) }
  if (key.name === "return" || key.name === "kpenter") {
    const row = state.rows.find((candidate) => candidate.id === state.actionRow)
    return row ? { type: "commit", id: state.actionId ?? "", row, value: state.editValue ?? "" } : { type: "update", state: toList(state) }
  }
  if (key.name === "backspace") return { type: "update", state: { ...state, editValue: (state.editValue ?? "").slice(0, -1) } }
  if (key.ctrl || key.meta) return { type: "none" }
  const text = key.sequence
  if (text.length === 1 && text >= " " && text !== "\x7f") return { type: "update", state: { ...state, editValue: (state.editValue ?? "") + text } }
  return { type: "none" }
}

function confirmModeKey(state: PickerState, key: KeyLike): PickerOutcome {
  if (key.name === "escape") return { type: "update", state: toList(state) }
  if (key.name === "return" || key.name === "kpenter") {
    const row = state.rows.find((candidate) => candidate.id === state.actionRow)
    return row ? { type: "commit", id: state.actionId ?? "", row } : { type: "update", state: toList(state) }
  }
  return { type: "none" }
}

/** A declared action whose key matches; `undefined` when none does. */
function matchAction(state: PickerState, key: KeyLike): PickerAction | undefined {
  if (key.meta || key.shift) return undefined
  return (state.actions ?? []).find((action) => action.key === key.name && Boolean(action.ctrl) === Boolean(key.ctrl))
}

export function pickerKey(state: PickerState, key: KeyLike): PickerOutcome {
  if (state.mode === "rename") return renameKey(state, key)
  if (state.mode === "confirm") return confirmModeKey(state, key)
  const rows = pickerRows(state)
  const action = matchAction(state, key)
  if (action) {
    const row = rows[state.index]
    if (!row) return { type: "none" }
    if (action.prompt === "value") {
      return { type: "update", state: { ...state, mode: "rename", actionRow: row.id, actionId: action.id, editValue: row.label } }
    }
    const template = action.confirmText ?? 'Delete "{label}"? Enter confirms · Esc cancels'
    return { type: "update", state: { ...state, mode: "confirm", actionRow: row.id, actionId: action.id, confirmText: template.replace("{label}", row.label) } }
  }
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

/** What a caller hands to `openPicker`: the title, rows, optional hint/actions, and what choosing a row (or committing an action) does. */
export interface PickerSpec {
  title: string
  rows: PickerRow[]
  hint?: string
  actions?: readonly PickerAction[]
  /** Most rows shown at once (default `pickerMaxRows`). */
  maxRows?: number
  /** Show the highlighted row's whole detail under the list. */
  detailPane?: boolean
  /** Runs after the picker closed (focus is back on the composer). */
  onSelect(row: PickerRow): void | Promise<void>
  /** Runs after a row action committed (`id` is the `PickerAction.id`; `value` is the edited text for `prompt: "value"`). */
  onAction?(id: string, row: PickerRow, value?: string): void | Promise<void>
}

/** An open picker: its list state plus what choosing a row (or committing an action) does. */
export interface ActivePicker extends PickerState {
  onSelect(row: PickerRow): void | Promise<void>
  onAction?(id: string, row: PickerRow, value?: string): void | Promise<void>
}
