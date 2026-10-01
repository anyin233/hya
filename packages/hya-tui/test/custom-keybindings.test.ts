import { afterEach, expect, test } from "bun:test"
import { parseShortcut, validateCustomKeybindings, customKeybindings, setCustomKeybindings, resolveCommandBinding } from "../src/keys/custom"
import { resolveBinding, type KeyLike } from "../src/keys/bindings"

afterEach(() => setCustomKeybindings({}))
const key = (name: string, modifiers: Partial<KeyLike> = {}): KeyLike => ({ name, ctrl: false, meta: false, shift: false, sequence: name, ...modifiers })

test("custom shortcuts preserve the full command and match exact modifiers, including Kitty Option", () => {
  setCustomKeybindings({ "alt+g": { command: "/tools on", scope: "conversation" }, F6: { command: "/layout focus left", scope: "workspace" } })
  expect(resolveCommandBinding(key("g", { option: true }))).toEqual({ command: "/tools on", scope: "conversation" })
  expect(resolveCommandBinding(key("g", { meta: true, shift: true }))).toBeUndefined()
  expect(resolveCommandBinding(key("g"))).toBeUndefined()
  expect(resolveCommandBinding(key("f6"))?.command).toBe("/layout focus left")
  expect(resolveBinding(key("c", { ctrl: true }))).toBe("quit")
  const copy = customKeybindings(); copy.F6!.command = "/exit"
  expect(customKeybindings().F6?.command).toBe("/layout focus left")
})

test("validation protects editing, browser shortcuts, built-ins and canonical duplicate assignments", () => {
  for (const label of ["a", "Enter", "Tab", "Ctrl+J", "Ctrl+W", "Ctrl+Shift+G", "Ctrl+C", "Alt+Left", "Ctrl+X", "F4"]) {
    expect(() => validateCustomKeybindings({ [label]: { command: "/tools", scope: "conversation" } })).toThrow()
  }
  expect(parseShortcut("option+arrowup").label).toBe("Alt+Up")
  expect(() => validateCustomKeybindings({ F6: { command: "/tools", scope: "pane" } })).toThrow("scope")
  expect(() => validateCustomKeybindings({ F6: { command: "hello", scope: "conversation" } })).toThrow("slash command")
  expect(() => validateCustomKeybindings({ F6: { command: "/tools\n/exit", scope: "conversation" } })).toThrow()
  expect(() => validateCustomKeybindings({ F6: { command: "/tools", scope: "conversation" }, f6: { command: "/exit", scope: "workspace" } })).toThrow("Duplicate")
})

test("rejected assignments leave current bindings intact; reset clears custom resolution", () => {
  setCustomKeybindings({ F6: { command: "/tools on", scope: "conversation" } })
  expect(() => setCustomKeybindings({ "Ctrl+C": { command: "/tools", scope: "conversation" } })).toThrow()
  expect(resolveCommandBinding(key("f6"))?.command).toBe("/tools on")
  setCustomKeybindings({})
  expect(resolveCommandBinding(key("f6"))).toBeUndefined()
})
