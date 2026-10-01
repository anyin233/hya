import { expect, test } from "bun:test"
import { composerKeyBindings, keyBindings, resolveBinding } from "../src/keys/bindings"

const key = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; sequence: string }> = {}) =>
  ({ name, ctrl: false, meta: false, shift: false, sequence: "", ...extra })

test("Tab completes; Ctrl+R has no app default", () => {
  expect(resolveBinding(key("tab"))).toBe("complete")
  expect(resolveBinding(key("", { sequence: "\t" }))).toBe("complete")
  expect(resolveBinding(key("r", { ctrl: true }))).toBeUndefined()
  expect(resolveBinding(key("r"))).toBeUndefined()
})

test("slash opens commands only with an empty message; Ctrl+X slash opens them with a draft", () => {
  expect(resolveBinding(key("/", { sequence: "/" }), { composerEmpty: true })).toBe("openCommands")
  expect(resolveBinding(key("/", { sequence: "/" }), { composerEmpty: false })).toBeUndefined()
  expect(resolveBinding(key("/", { sequence: "/" }), { chord: "ctrl+x", composerEmpty: false })).toBe("openCommands")
})

test("maps pane navigation and transcript scrolling; optional toggles are unassigned", () => {
  for (const [direction, action] of [["left", "focusPaneLeft"], ["right", "focusPaneRight"], ["up", "focusPaneUp"], ["down", "focusPaneDown"]] as const) {
    expect(resolveBinding(key(direction, { meta: true }))).toBe(action)
  }
  expect(resolveBinding(key("b", { ctrl: true }))).toBeUndefined()
  expect(resolveBinding(key("o", { ctrl: true }))).toBeUndefined()
  expect(resolveBinding(key("g", { ctrl: true }))).toBeUndefined()
  expect(resolveBinding(key("g"))).toBeUndefined()
  expect(resolveBinding(key("b"))).toBeUndefined()
  expect(resolveBinding(key("pageup"))).toBe("pageUp")
  expect(resolveBinding(key("pagedown"))).toBe("pageDown")
  expect(resolveBinding(key("home", { ctrl: true }), { composerEmpty: true })).toBeUndefined()
  expect(resolveBinding(key("end", { ctrl: true }), { composerEmpty: true })).toBeUndefined()
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
    key("pageup"), key("pagedown"), key("home"), key("end"),
    key("escape"), key("c", { ctrl: true }), key("d", { ctrl: true }), key("tab", { shift: true, sequence: "\x1b[Z" }),
    key("?", { shift: true, sequence: "?" }), key("x", { ctrl: true }), key("f4"),
    key("p", { ctrl: true }), key("/", { sequence: "/" }),
    key("left", { meta: true }), key("right", { meta: true }), key("up", { meta: true }), key("down", { meta: true }),
  ]
  const reachable = new Set(probes.filter((probe) => !browserReserved.some((reserved) => reserved(probe)))
    .map((probe) => resolveBinding(probe, { composerEmpty: probe.name === "d" || probe.name === "?" || probe.name === "/" || probe.name === "home" || probe.name === "end" })))
  // The second key of the Ctrl+X chord.
  reachable.add(resolveBinding(key("e", { ctrl: true }), { chord: "ctrl+x" }))
  for (const name of ["u", "r", "f"]) reachable.add(resolveBinding(key(name, { sequence: name }), { chord: "ctrl+x" }))
  expect([...new Set(keyBindings.map((binding) => binding.action))].sort())
    .toEqual(["chord", "complete", "eof", "focusPaneDown", "focusPaneLeft", "focusPaneRight", "focusPaneUp", "help", "interrupt", "openCommands", "pageDown", "pageUp", "quit", "scrollBottom", "scrollTop"])
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

test("Shift+Tab has no global default; plain Tab still completes", () => {
  expect(resolveBinding(key("tab", { shift: true, sequence: "\x1b[Z" }))).toBeUndefined()
  expect(resolveBinding(key("tab", { sequence: "\t" }))).toBe("complete")
})

test("Ctrl+X starts the command prefix; the old editor chord is unassigned", () => {
  expect(resolveBinding(key("x", { ctrl: true }))).toBe("chord")
  expect(resolveBinding(key("e", { ctrl: true }), { chord: "ctrl+x" })).toBeUndefined()
  expect(resolveBinding(key("e", { sequence: "e" }), { chord: "ctrl+x" })).toBeUndefined()
  // Without the prefix Ctrl+E stays the editor's end-of-line key, and e types.
  expect(resolveBinding(key("e", { ctrl: true }))).toBeUndefined()
  expect(resolveBinding(key("e", { sequence: "e" }))).toBeUndefined()
})

test("old undo, redo and fork chords are unassigned", () => {
  expect(resolveBinding(key("u", { sequence: "u" }), { chord: "ctrl+x" })).toBeUndefined()
  expect(resolveBinding(key("u", { ctrl: true }), { chord: "ctrl+x" })).toBeUndefined()
  expect(resolveBinding(key("r", { sequence: "r" }), { chord: "ctrl+x" })).toBeUndefined()
  // Ctrl+R after the prefix is redo, not refresh.
  expect(resolveBinding(key("r", { ctrl: true }), { chord: "ctrl+x" })).toBeUndefined()
  expect(resolveBinding(key("f", { sequence: "f" }), { chord: "ctrl+x" })).toBeUndefined()
  expect(resolveBinding(key("u", { sequence: "u" }))).toBeUndefined()
  expect(resolveBinding(key("r", { ctrl: true }))).toBeUndefined()
})

test("optional app shortcuts are all unassigned", () => {
  for (const name of ["r", "b", "p", "o", "g"]) expect(resolveBinding(key(name, { ctrl: true }))).toBeUndefined()
  expect(resolveBinding(key("f4"))).toBeUndefined()
  expect(resolveBinding(key("tab", { ctrl: true }))).toBeUndefined()
})
