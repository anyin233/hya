import { expect, test } from "bun:test"
import { keyBindings, resolveBinding } from "../src/keys/bindings"

const key = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; sequence: string }> = {}) =>
  ({ name, ctrl: false, meta: false, shift: false, sequence: "", ...extra })

test("maps Tab to completion and Ctrl+R to refresh", () => {
  expect(resolveBinding(key("tab"))).toBe("complete")
  expect(resolveBinding(key("", { sequence: "\t" }))).toBe("complete")
  expect(resolveBinding(key("r", { ctrl: true }))).toBe("refresh")
  expect(resolveBinding(key("r"))).toBeUndefined()
})

test("every binding is documented and none relies only on browser-reserved keys", () => {
  for (const binding of keyBindings) {
    expect(binding.description.length).toBeGreaterThan(0)
    expect(binding.label.length).toBeGreaterThan(0)
  }
  expect(keyBindings.map((binding) => binding.action).sort()).toEqual(["complete", "refresh"])
})
