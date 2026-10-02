import { afterEach, expect, test } from "bun:test"
import { activeInheritedBindingRows, inheritedBindingRows } from "../src/keys/inventory"
import { setCustomKeybindings, validateCustomKeybindings } from "../src/keys/custom"

afterEach(() => setCustomKeybindings({}))

test("inherited and contextual keys are inspectable, including Vim prefixes and named editor keys", () => {
  const rows = inheritedBindingRows()
  expect(rows.find((row) => row.shortcut === "Ctrl+W")?.label).toBe("delete-word-backward")
  expect(rows.some((row) => row.shortcut === "G" && row.label.includes("gg prefix"))).toBe(true)
  expect(rows.some((row) => row.shortcut === "F2" && row.label.includes("rename"))).toBe(true)
  expect(rows.some((row) => row.shortcut === "Enter" && row.tag === "pane" && row.label.startsWith("Layout:"))).toBe(true)
  expect(validateCustomKeybindings({ KeypadEnter: null })).toEqual({ KeypadEnter: null })
})

test("disabled keys leave the active list while administrative command keys remain visible", () => {
  setCustomKeybindings({ "Ctrl+U": null, F2: null, Enter: null })
  const rows = activeInheritedBindingRows()
  expect(rows.some((row) => row.shortcut === "Ctrl+U" || row.shortcut === "F2")).toBe(false)
  expect(rows.filter((row) => row.shortcut === "Enter").every((row) => row.id.startsWith("context:Command:"))).toBe(true)
  expect(rows.some((row) => row.shortcut === "Enter")).toBe(true)
})
