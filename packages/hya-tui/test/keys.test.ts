import { expect, test } from "bun:test"
import { composerKeyBindings, keyBindings, resolveBinding } from "../src/keys/bindings"

const key = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; sequence: string }> = {}) =>
  ({ name, ctrl: false, meta: false, shift: false, sequence: "", ...extra })

test("maps Tab to completion and Ctrl+R to refresh", () => {
  expect(resolveBinding(key("tab"))).toBe("complete")
  expect(resolveBinding(key("", { sequence: "\t" }))).toBe("complete")
  expect(resolveBinding(key("r", { ctrl: true }))).toBe("refresh")
  expect(resolveBinding(key("r"))).toBeUndefined()
})

test("maps the layout, reasoning, and transcript scrolling keys", () => {
  expect(resolveBinding(key("b", { ctrl: true }))).toBe("toggleSidebar")
  expect(resolveBinding(key("o", { ctrl: true }))).toBe("toggleThinking")
  expect(resolveBinding(key("g", { ctrl: true }))).toBe("toggleTools")
  expect(resolveBinding(key("g"))).toBeUndefined()
  expect(resolveBinding(key("b"))).toBeUndefined()
  expect(resolveBinding(key("pageup"))).toBe("pageUp")
  expect(resolveBinding(key("pagedown"))).toBe("pageDown")
  expect(resolveBinding(key("home", { ctrl: true }))).toBe("scrollTop")
  expect(resolveBinding(key("end", { ctrl: true }))).toBe("scrollBottom")
})

test("plain Home and End scroll the transcript only while the composer is empty", () => {
  expect(resolveBinding(key("home"), { composerEmpty: true })).toBe("scrollTop")
  expect(resolveBinding(key("end"), { composerEmpty: true })).toBe("scrollBottom")
  expect(resolveBinding(key("home"), { composerEmpty: false })).toBeUndefined()
  expect(resolveBinding(key("end"))).toBeUndefined()
})

const browserReserved = [
  (k: ReturnType<typeof key>) => k.ctrl && ["w", "t", "n", "l", "tab"].includes(k.name),
  (k: ReturnType<typeof key>) => k.name === "tab" && k.ctrl,
]

test("every binding is documented and reachable without a browser-reserved shortcut", () => {
  for (const binding of keyBindings) {
    expect(binding.description.length).toBeGreaterThan(0)
    expect(binding.label.length).toBeGreaterThan(0)
  }
  const probes = [
    key("tab"), key("r", { ctrl: true }), key("b", { ctrl: true }), key("o", { ctrl: true }), key("g", { ctrl: true }),
    key("pageup"), key("pagedown"), key("home", { ctrl: true }), key("end", { ctrl: true }),
    key("escape"), key("c", { ctrl: true }), key("d", { ctrl: true }), key("tab", { shift: true, sequence: "\x1b[Z" }),
    key("?", { shift: true, sequence: "?" }), key("x", { ctrl: true }),
  ]
  const reachable = new Set(probes.filter((probe) => !browserReserved.some((reserved) => reserved(probe)))
    .map((probe) => resolveBinding(probe, { composerEmpty: probe.name === "d" || probe.name === "?" })))
  // The second key of the Ctrl+X chord.
  reachable.add(resolveBinding(key("e", { ctrl: true }), { chord: "ctrl+x" }))
  expect([...new Set(keyBindings.map((binding) => binding.action))].sort())
    .toEqual(["chord", "complete", "cycleMode", "eof", "externalEditor", "help", "interrupt", "pageDown", "pageUp", "quit", "refresh", "scrollBottom", "scrollTop", "toggleSidebar", "toggleThinking", "toggleTools"])
  for (const binding of keyBindings) expect(reachable.has(binding.action)).toBe(true)
})

test("Esc interrupts, Ctrl+C quits, and Ctrl+D quits only on an empty input", () => {
  expect(resolveBinding(key("escape"))).toBe("interrupt")
  expect(resolveBinding(key("c", { ctrl: true }))).toBe("quit")
  expect(resolveBinding(key("c"))).toBeUndefined()
  expect(resolveBinding(key("d", { ctrl: true }), { composerEmpty: true })).toBe("eof")
  // With text, Ctrl+D stays the editor's forward delete.
  expect(resolveBinding(key("d", { ctrl: true }), { composerEmpty: false })).toBeUndefined()
})

test("the composer submits on Enter and inserts a newline on Ctrl+J, Shift+Enter, and Alt+Enter", () => {
  const action = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; shift: boolean }> = {}) =>
    composerKeyBindings.find((binding) =>
      binding.name === name && !!binding.ctrl === !!extra.ctrl && !!binding.meta === !!extra.meta && !!binding.shift === !!extra.shift)?.action
  expect(action("return")).toBe("submit")
  expect(action("kpenter")).toBe("submit")
  expect(action("linefeed")).toBe("newline")
  expect(action("j", { ctrl: true })).toBe("newline")
  expect(action("return", { shift: true })).toBe("newline")
  expect(action("return", { meta: true })).toBe("newline")
  expect(action("home")).toBe("visual-line-home")
  expect(action("end")).toBe("visual-line-end")
})

test("Shift+Tab (CSI Z) cycles the permission mode; plain Tab still completes", () => {
  expect(resolveBinding(key("tab", { shift: true, sequence: "\x1b[Z" }))).toBe("cycleMode")
  expect(resolveBinding(key("tab", { sequence: "\t" }))).toBe("complete")
})

test("Ctrl+X starts a chord; Ctrl+E (or E) after it opens the external editor", () => {
  expect(resolveBinding(key("x", { ctrl: true }))).toBe("chord")
  expect(resolveBinding(key("e", { ctrl: true }), { chord: "ctrl+x" })).toBe("externalEditor")
  expect(resolveBinding(key("e", { sequence: "e" }), { chord: "ctrl+x" })).toBe("externalEditor")
  // Without the prefix Ctrl+E stays the editor's end-of-line key, and e types.
  expect(resolveBinding(key("e", { ctrl: true }))).toBeUndefined()
  expect(resolveBinding(key("e", { sequence: "e" }))).toBeUndefined()
  expect(keyBindings.find((binding) => binding.action === "externalEditor")?.label).toBe("Ctrl+X Ctrl+E")
})
