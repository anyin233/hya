import { expect, test } from "bun:test"
import { mergeCommandEntries, nativeCommandSpecs } from "../src/commands"
import { composerKeyLabel, helpGroups, helpPickerRows, helpRows, keyHelpText } from "../src/commands/help"
import { composerKeyBindings, keyBindings, resolveBinding } from "../src/keys/bindings"
import { sessionPickerActions } from "../src/commands/native"
import { providerKeyRows } from "../src/state/providers"

const entries = mergeCommandEntries(nativeCommandSpecs, [
  { name: "review", description: "Review changes", source: "command" },
  { name: "deploy-check", description: "Check a deploy", source: "skill" },
])

test("every global key binding appears in help, in its group, with its description", () => {
  const rows = helpRows(entries)
  for (const binding of keyBindings) {
    const row = rows.find((candidate) => candidate.keys === binding.label && candidate.description === binding.description)
    expect(row, `help lists ${binding.label}`).toBeDefined()
    expect(helpGroups).toContain(row!.group)
  }
})

test("every composer editing binding appears in help; Shift+Enter is marked terminal only", () => {
  const rows = helpRows(entries)
  for (const binding of composerKeyBindings) {
    const label = composerKeyLabel(binding)
    expect(rows.some((row) => row.group === "Composer" && row.keys.split(" / ").includes(label)), `help lists ${label}`).toBe(true)
  }
  expect(composerKeyLabel({ name: "return", shift: true, action: "newline" })).toBe("Shift+Enter")
  expect(composerKeyLabel({ name: "j", ctrl: true, action: "newline" })).toBe("Ctrl+J")
  expect(composerKeyLabel({ name: "return", meta: true, action: "newline" })).toBe("Alt+Enter")
  const shiftEnter = rows.find((row) => row.keys.split(" / ").includes("Shift+Enter"))!
  expect(shiftEnter.description).toContain("terminal only")
})

test("prompt keys, picker keys and row actions, and every command (local, server, skill) are in help", () => {
  const rows = helpRows(entries)
  expect(rows.some((row) => row.group === "Prompts" && row.keys.includes("1"))).toBe(true)
  expect(rows.some((row) => row.group === "Pickers" && row.keys.includes("Esc"))).toBe(true)
  for (const action of sessionPickerActions) expect(rows.some((row) => row.group === "Pickers" && row.description.includes(action.label))).toBe(true)
  for (const spec of nativeCommandSpecs) {
    expect(rows.some((row) => row.group === "Commands" && row.keys.startsWith(spec.name) && row.source === "local"), spec.name).toBe(true)
  }
  expect(rows.find((row) => row.keys.startsWith("/review"))?.source).toBe("server")
  expect(rows.find((row) => row.keys.startsWith("/deploy-check"))?.source).toBe("skill")
})

test("help rows are grouped in the fixed group order", () => {
  const rows = helpRows(entries)
  const order = rows.map((row) => helpGroups.indexOf(row.group))
  expect(order).toEqual([...order].sort((a, b) => a - b))
  expect(new Set(rows.map((row) => row.group)).size).toBe(helpGroups.length)
})

test("picker rows tag keys by group and commands by source; ids are unique", () => {
  const rows = helpPickerRows(entries)
  expect(new Set(rows.map((row) => row.id)).size).toBe(rows.length)
  expect(rows.find((row) => row.label === "Ctrl+B")?.tag).toBe("views")
  expect(rows.find((row) => row.label.startsWith("/review"))?.tag).toBe("server")
})

test("? opens help only while the composer is empty", () => {
  const key = { name: "?", ctrl: false, meta: false, shift: true, sequence: "?" }
  expect(resolveBinding(key, { composerEmpty: true })).toBe("help")
  expect(resolveBinding(key, { composerEmpty: false })).toBeUndefined()
  expect(resolveBinding({ ...key, name: "/", sequence: "/" }, { composerEmpty: true })).toBeUndefined()
})

test("the key help text (connection-failure view) is generated from the same rows", () => {
  const text = keyHelpText()
  for (const binding of keyBindings) expect(text).toContain(binding.label)
  expect(text).toContain("Shift+Enter")
})

test("help lists the external editor chord, mouse copy, and the vim normal-mode keys", () => {
  const rows = helpRows(entries)
  expect(rows.find((row) => row.keys === "Ctrl+X Ctrl+E")?.group).toBe("Composer")
  expect(rows.some((row) => row.group === "Transcript" && row.keys.startsWith("Mouse drag") && row.description.includes("clipboard"))).toBe(true)
  const vim = rows.filter((row) => row.group === "Vim")
  for (const keys of ["h j k l", "w b e", "0 ^ $", "gg / G", "i a I A o O", "x / dd / D", "u / Ctrl+R", "Enter", "Esc"]) {
    expect(vim.some((row) => row.keys === keys), keys).toBe(true)
  }
})

test("help lists the Provider View keys in their own group", () => {
  const rows = helpRows(entries).filter((row) => row.group === "Providers")
  for (const row of providerKeyRows) expect(rows.some((candidate) => candidate.keys === row.keys && candidate.description === row.description), row.keys).toBe(true)
  expect(rows.find((row) => row.keys === "t")?.description).toContain("Test the highlighted model")
})
