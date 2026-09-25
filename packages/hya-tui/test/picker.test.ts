import { expect, test } from "bun:test"
import { createPicker, pickerKey, pickerMatches, pickerRows, pickerWindow, type PickerRow, type PickerState } from "../src/state/picker"

const key = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; sequence: string }> = {}) =>
  ({ name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra })

const rows: PickerRow[] = [
  { id: "manual", label: "Manual", tag: "builtin", detail: "Ask the user before actions that need permission." },
  { id: "yolo", label: "Yolo", tag: "builtin", detail: "Allow every action without asking.", current: true },
  { id: "e2e/approver/echo-only", label: "Echo only", tag: "e2e/approver", detail: "Approve echo commands" },
]

function update(state: PickerState, name: string, extra = {}): PickerState {
  const outcome = pickerKey(state, key(name, extra))
  if (outcome.type !== "update") throw new Error(`expected update, got ${outcome.type}`)
  return outcome.state
}

test("a new picker highlights the current row, else the first", () => {
  expect(createPicker({ title: "Permission mode", rows }).index).toBe(1)
  expect(createPicker({ title: "x", rows: rows.map((row) => ({ ...row, current: false })) }).index).toBe(0)
  expect(createPicker({ title: "x", rows: [] }).index).toBe(0)
})

test("filtering matches every typed word against label, id, tag, and detail, case-insensitively", () => {
  expect(pickerMatches(rows, "").map((row) => row.id)).toEqual(["manual", "yolo", "e2e/approver/echo-only"])
  expect(pickerMatches(rows, "YO").map((row) => row.id)).toEqual(["yolo"])
  expect(pickerMatches(rows, "approver").map((row) => row.id)).toEqual(["e2e/approver/echo-only"])
  expect(pickerMatches(rows, "builtin ask").map((row) => row.id)).toEqual(["manual", "yolo"])
  expect(pickerMatches(rows, "nothing-matches")).toEqual([])
})

test("typing filters and resets the highlight; Backspace widens again", () => {
  let state = createPicker({ title: "Permission mode", rows })
  state = update(state, "e")
  state = update(state, "c")
  expect(state.query).toBe("ec")
  expect(pickerRows(state).map((row) => row.id)).toEqual(["e2e/approver/echo-only"])
  expect(state.index).toBe(0)
  state = update(state, "backspace")
  state = update(state, "backspace")
  expect(state.query).toBe("")
  // Back to the full list: the current row is highlighted again.
  expect(state.index).toBe(1)
  // Ctrl+U clears the filter.
  state = update(update(state, "x"), "u", { ctrl: true })
  expect(state.query).toBe("")
})

test("Up/Down (and Shift+Tab/Tab) move the highlight with wrap-around", () => {
  let state = createPicker({ title: "x", rows })
  state = update(state, "down")
  expect(state.index).toBe(2)
  state = update(state, "down")
  expect(state.index).toBe(0)
  state = update(state, "up")
  expect(state.index).toBe(2)
  state = update(state, "tab", { shift: true, sequence: "\x1b[Z" })
  expect(state.index).toBe(1)
  state = update(state, "tab", { sequence: "\t" })
  expect(state.index).toBe(2)
})

test("Enter selects the highlighted row, Esc closes, other keys are swallowed", () => {
  const state = createPicker({ title: "x", rows })
  expect(pickerKey(state, key("return"))).toEqual({ type: "select", row: rows[1]! })
  expect(pickerKey(state, key("escape"))).toEqual({ type: "close" })
  expect(pickerKey(state, key("f5"))).toEqual({ type: "none" })
  // Enter with no match selects nothing.
  const empty = update(update(state, "z"), "z")
  expect(pickerRows(empty)).toEqual([])
  expect(pickerKey(empty, key("return"))).toEqual({ type: "none" })
})

test("the visible window keeps the highlight in view", () => {
  expect(pickerWindow(3, 0, 8)).toEqual({ start: 0, end: 3 })
  expect(pickerWindow(20, 0, 5)).toEqual({ start: 0, end: 5 })
  expect(pickerWindow(20, 7, 5)).toEqual({ start: 3, end: 8 })
  expect(pickerWindow(20, 19, 5)).toEqual({ start: 15, end: 20 })
})
