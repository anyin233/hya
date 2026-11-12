import { expect, test } from "bun:test"
import { HOOK_NAMES, isHookParams, isWireCompletionRequest } from "../src/wire.gen"

test("every native hook accepts the Rust golden payload without rewriting it", async () => {
  const fixtures = await Bun.file(new URL("../../../hya-plugin/tests/fixtures/hook-wire.json", import.meta.url)).json()
  expect(Object.keys(fixtures).sort()).toEqual([...HOOK_NAMES].sort())
  for (const name of HOOK_NAMES) {
    for (const params of [fixtures[name].params, ...(fixtures[name].other_params ?? [])]) {
      const before = JSON.stringify(params)
      expect(isHookParams(name, params)).toBe(true)
      expect(JSON.stringify(params)).toBe(before)
    }
  }
})

test("generated payload guards reject wrong nested fields", () => {
  expect(isHookParams("tool.execute.before", { session: "s", message: "m", call: "c", tool: "echo" })).toBe(false)
  expect(isHookParams("permission.ask", { action: "read", resource: { type: "path" } })).toBe(false)
  expect(isHookParams("compaction.before", { session: "s", trigger: "overflow", messages_token_estimate: 10 })).toBe(false)
  expect(isWireCompletionRequest({ model: "dev/echo", messages: [], tools: [], headers: { "x-test": 5 } })).toBe(false)
})

test("optional completion fields accept explicit null as Rust serde does", () => {
  const request: import("../src/wire.gen").WireCompletionRequest = {
    model: "dev/echo", messages: [], tools: [], system: null,
  }
  expect(isWireCompletionRequest(request)).toBe(true)
})
