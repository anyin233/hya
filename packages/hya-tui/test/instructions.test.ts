import { expect, test } from "bun:test"
import { footerInstruction } from "../src/instructions"

test("bottom instructions tell users what to do next in each view", () => {
  expect(footerInstruction("chat")).toContain("Enter a prompt")
  expect(footerInstruction("models")).toContain("/key opens the Provider View")
})

test("a subagent's read-only view says how to get back", () => {
  expect(footerInstruction("chat", true)).toBe("Read-only subagent view · Esc returns to the parent · click a task card or /open <n> to switch")
  // Other views keep their own instruction.
  expect(footerInstruction("help", true)).toContain("Tab completes")
})
