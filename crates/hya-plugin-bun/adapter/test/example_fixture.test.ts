import { afterEach, expect, test } from "bun:test"
import path from "node:path"

import {
  cleanupTempDirs,
  initializeRequest,
  makeTempDir,
  runAdapterProcess,
  shutdownRequest,
} from "./helpers"

afterEach(async () => {
  await cleanupTempDirs()
})

/**
 * Fixture module shaped exactly like
 * `docs/examples/bun-transient/extensions/runtime.js` (verbatim copy).
 */
const exampleRuntime = path.join(import.meta.dir, "fixtures", "example_runtime.js")

test("loads the documented example extension shape and executes its tool", async () => {
  const responses = await runAdapterProcess(
    [
      initializeRequest(1, {
        activation_id: "activation-example-test",
        lifecycle: "transient",
      }),
      {
        jsonrpc: "2.0",
        id: 2,
        method: "tool/call",
        params: {
          tool: "echo",
          session: "session-1",
          call: "call-1",
          input: { value: [1, 2, 3] },
        },
      },
      shutdownRequest(3),
    ],
    { argv: ["--bundle-extension", exampleRuntime] },
  )

  expect(responses).toHaveLength(3)
  expect(responses[0]?.result).toMatchObject({
    plugin: { id: "bun", kind: "bun" },
    tools: [
      {
        name: "echo",
        description: "Return the provided value as JSON.",
        inputSchema: { type: "object", properties: {}, required: [] },
      },
    ],
    hooks: [],
  })
  expect(responses[1]?.result).toEqual({
    ok: true,
    output: {
      title: "",
      output: '{"value":[1,2,3]}',
      metadata: {},
    },
    time_ms: expect.any(Number),
  })
})

test("loads the documented disjoint example shape declaring tool and event", async () => {
  const root = await makeTempDir()
  const alpha = path.join(root, "alpha.js")
  await Bun.write(
    alpha,
    [
      "export default {",
      '  id: "docs-bun-alpha-extension",',
      "  server: async () => ({",
      "    tool: {",
      "      echo: {",
      '        description: "Return input as deterministic JSON text.",',
      '        execute: async (input) => JSON.stringify(input) ?? "",',
      "      },",
      "    },",
      "    event: async () => {},",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(11, {
        activation_id: "activation-disjoint-test",
        lifecycle: "transient",
      }),
      shutdownRequest(12),
    ],
    { argv: ["--bundle-extension", alpha] },
  )

  expect(responses[0]?.result).toMatchObject({
    tools: [
      {
        name: "echo",
        description: "Return input as deterministic JSON text.",
      },
    ],
    hooks: [{ name: "event" }],
  })
})
