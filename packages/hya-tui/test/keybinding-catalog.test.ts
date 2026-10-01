import { expect, test } from "bun:test"
import { keyBindings } from "../src/keys/bindings"
import { bindingSettings, findBindingSetting } from "../src/keys/catalog"

test("the settings catalog groups every active shortcut once by stable action", () => {
  const entries = bindingSettings()
  expect(entries.filter((entry) => entry.keys.length).map((entry) => entry.id).sort()).toEqual([...new Set(keyBindings.map((binding) => binding.action))].sort())
  for (const entry of entries) {
    const bindings = keyBindings.filter((binding) => binding.action === entry.id)
    expect(entry.keys).toEqual(bindings.map((binding) => binding.label))
    for (const binding of bindings) expect(entry.description).toContain(binding.description)
  }
  expect(findBindingSetting("openCommands")?.keys).toEqual(["Ctrl+X /", "/"])
})

test("command lookup retains quit guards and exposes pane routing context", () => {
  expect(findBindingSetting("/exit")).toEqual(findBindingSetting("quit"))
  expect(findBindingSetting("quit")).toMatchObject({ command: "/exit", scope: "workspace", keys: ["Ctrl+C"] })
  expect(findBindingSetting("quit")?.description).toContain("press again within 2 s")
  expect(findBindingSetting("toggleTools")?.context).toContain("Conversation focus only")
  expect(findBindingSetting("pageUp")?.scope).toBe("pane")
  expect(findBindingSetting("unknown")).toBeUndefined()
  // Inspection must not mutate or arm the actual binding table.
  const before = keyBindings.map((binding) => binding.label)
  findBindingSetting("chord")
  expect(keyBindings.map((binding) => binding.label)).toEqual(before)
})

test("unassigned commands preserve their scope for custom bindings", () => {
  expect(findBindingSetting("/refresh")).toMatchObject({ keys: [], scope: "workspace" })
  expect(findBindingSetting("/tools")).toMatchObject({ keys: [], scope: "conversation" })
  expect(findBindingSetting("/pending")).toMatchObject({ keys: [], scope: "workspace" })
})
