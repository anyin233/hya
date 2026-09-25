import { expect, test } from "bun:test"
import { footerInstruction } from "../src/instructions"

test("bottom instructions tell users what to do after opening saved keys", () => {
  expect(footerInstruction("keys", true)).toContain("/key set <provider>")
  expect(footerInstruction("keys", true)).toContain("/key remove <provider>")
  expect(footerInstruction("keys", false)).toContain("restart backend")
  expect(footerInstruction("keys", true, true)).toContain("Enter saves")
  expect(footerInstruction("keys", true, true)).toContain("Esc cancels")
  expect(footerInstruction("chat", true)).toContain("Enter a prompt")
})

test("a subagent's read-only view says how to get back", () => {
  expect(footerInstruction("chat", true, false, true)).toBe("Read-only subagent view · Esc returns to the parent · click a task card or /open <n> to switch")
  // Other views keep their own instruction.
  expect(footerInstruction("help", true, false, true)).toContain("Tab completes")
})
