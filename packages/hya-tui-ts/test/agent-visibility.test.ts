import { expect, test } from "bun:test"
import {
  isSubagentAutocompleteAgent,
  isTuiSelectableAgent,
} from "../src/upstream/util/agent-visibility"

test("TUI selector contains only role-mapped main (mode primary)", () => {
  const agents = [
    { id: "build", mode: "primary", hidden: false },
    { id: "plan", mode: "primary", hidden: false },
    { id: "research", mode: "subagent", hidden: false },
    { id: "compaction", mode: "subagent", hidden: true },
  ]
  const selector = agents.filter(isTuiSelectableAgent).map((agent) => agent.id)
  expect(selector).toEqual(["build", "plan"])
  // Reachable subagent is visible elsewhere but never TUI-selectable.
  expect(isTuiSelectableAgent({ mode: "subagent" })).toBe(false)
  // hidden must not be required for selector decisions.
  expect(isTuiSelectableAgent({ mode: "primary" })).toBe(true)
})

test("subagent autocomplete keeps reachable subagents and excludes fixed system", () => {
  expect(
    isSubagentAutocompleteAgent({ mode: "subagent", hidden: false }),
  ).toBe(true)
  expect(isSubagentAutocompleteAgent({ mode: "subagent", hidden: true })).toBe(false)
  expect(isSubagentAutocompleteAgent({ mode: "primary", hidden: false })).toBe(false)
})
