import { expect, test } from "bun:test"
import {
  confirmKey,
  modeCycle,
  modeDisplay,
  modeNotice,
  modeRows,
  nextMode,
  requestMode,
  yoloConfirmText,
  type PermissionModeInfo,
} from "../src/state/modes"
import { statusLineText } from "../src/state/activity"

const key = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; sequence: string }> = {}) =>
  ({ name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra })
const shiftTab = key("tab", { shift: true, sequence: "\x1b[Z" })

const builtin: PermissionModeInfo[] = [
  { id: "manual", title: "Manual", description: "Ask the user", source: "builtin" },
  { id: "yolo", title: "Yolo", description: "Allow everything", source: "builtin" },
]
const plugin: PermissionModeInfo[] = [
  ...builtin,
  { id: "acme/approver/careful", title: "Careful", description: "Approve read-only commands", source: "acme/approver" },
  { id: "acme/approver/ci", title: "CI", source: "acme/approver" },
]

test("the cycle is manual → yolo → bundle modes in listing order, and wraps", () => {
  expect(modeCycle(builtin)).toEqual(["manual", "yolo"])
  expect(modeCycle(plugin)).toEqual(["manual", "yolo", "acme/approver/careful", "acme/approver/ci"])
  // No listing (not loaded yet, or an older backend): the built-ins.
  expect(modeCycle([])).toEqual(["manual", "yolo"])
  // Built-ins come first even if the listing orders them differently.
  expect(modeCycle([plugin[2]!, plugin[1]!, plugin[0]!])).toEqual(["manual", "yolo", "acme/approver/careful"])
  const cycle = modeCycle(plugin)
  expect(nextMode("manual", cycle)).toBe("yolo")
  expect(nextMode("yolo", cycle)).toBe("acme/approver/careful")
  expect(nextMode("acme/approver/ci", cycle)).toBe("manual")
  expect(nextMode("yolo", modeCycle(builtin))).toBe("manual")
  // An unknown current mode (a removed bundle's) restarts at manual.
  expect(nextMode("gone/bundle/mode", cycle)).toBe("manual")
})

test("switching to yolo needs a confirmation until it was confirmed once", () => {
  expect(requestMode("yolo", "manual", false)).toEqual({ type: "confirm", confirm: { target: "yolo", from: "manual" } })
  expect(requestMode("yolo", "manual", true)).toEqual({ type: "apply", mode: "yolo" })
  expect(requestMode("manual", "yolo", false)).toEqual({ type: "apply", mode: "manual" })
  expect(requestMode("acme/approver/careful", "manual", false)).toEqual({ type: "apply", mode: "acme/approver/careful" })
  expect(requestMode("manual", "manual", false)).toEqual({ type: "same" })
  expect(yoloConfirmText).toBe("Enable yolo? Every tool call runs without asking · Enter confirms · Esc cancels")
})

test("the confirmation takes Enter, Esc, and Shift+Tab; any other key cancels it and passes through", () => {
  const confirm = { target: "yolo", from: "manual" }
  expect(confirmKey(key("return"), confirm, modeCycle(plugin))).toEqual({ type: "confirm" })
  expect(confirmKey(key("kpenter"), confirm, modeCycle(plugin))).toEqual({ type: "confirm" })
  expect(confirmKey(key("escape"), confirm, modeCycle(plugin))).toEqual({ type: "cancel" })
  // Shift+Tab again skips past yolo to the next mode of the cycle ...
  expect(confirmKey(shiftTab, confirm, modeCycle(plugin))).toEqual({ type: "advance", mode: "acme/approver/careful" })
  // ... which, with only the built-ins, is where it started: nothing changes.
  expect(confirmKey(shiftTab, confirm, modeCycle(builtin))).toEqual({ type: "cancel" })
  expect(confirmKey(key("a"), confirm, modeCycle(plugin))).toEqual({ type: "pass" })
  expect(confirmKey(key("1"), confirm, modeCycle(plugin))).toEqual({ type: "pass" })
})

test("each mode has its status bar text and tone", () => {
  expect(modeDisplay("manual", plugin)).toEqual({ text: "manual", tone: "normal" })
  expect(modeDisplay("", plugin)).toEqual({ text: "manual", tone: "normal" })
  expect(modeDisplay("yolo", plugin)).toEqual({ text: "⚠ yolo", tone: "error" })
  expect(modeDisplay("acme/approver/careful", plugin)).toEqual({ text: "Careful", tone: "accent" })
  // A bundle mode missing from the listing shows its id.
  expect(modeDisplay("gone/b/m", builtin)).toEqual({ text: "gone/b/m", tone: "accent" })
})

test("the transcript notice names the new mode", () => {
  expect(modeNotice("yolo", plugin)).toBe("Permission mode → yolo")
  expect(modeNotice("manual", plugin)).toBe("Permission mode → manual")
  expect(modeNotice("acme/approver/careful", plugin)).toBe("Permission mode → Careful (acme/approver/careful)")
})

test("picker rows show title, source, description, and mark the current mode", () => {
  expect(modeRows(plugin, "yolo")).toEqual([
    { id: "manual", label: "Manual", tag: "builtin", detail: "Ask the user", current: false },
    { id: "yolo", label: "Yolo", tag: "builtin", detail: "Allow everything", current: true },
    { id: "acme/approver/careful", label: "Careful", tag: "acme/approver", detail: "Approve read-only commands", current: false },
    { id: "acme/approver/ci", label: "CI", tag: "acme/approver", detail: "", current: false },
  ])
  // Without a listing, the built-ins are offered.
  expect(modeRows([], "manual").map((row) => row.id)).toEqual(["manual", "yolo"])
})

test("the status line drops turn-progress text while the working line shows the run", () => {
  expect(statusLineText("Running · msg_1", true)).toBe("")
  expect(statusLineText("Running · msg_1 · 2 queued", true)).toBe("")
  expect(statusLineText("Running shell · ls", true)).toBe("")
  expect(statusLineText("Sending prompt…", true)).toBe("")
  expect(statusLineText("Queued · 1 waiting", true)).toBe("")
  // Other messages stay, and nothing is hidden without the working line.
  expect(statusLineText("Permission mode yolo", true)).toBe("Permission mode yolo")
  expect(statusLineText("Error: boom", true)).toBe("Error: boom")
  expect(statusLineText("Running · msg_1", false)).toBe("Running · msg_1")
  expect(statusLineText("Ready", false)).toBe("Ready")
})
