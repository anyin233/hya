import { expect, test } from "bun:test"

import {
  buildToolRegistry,
  callRegisteredTool,
  type ToolContext,
} from "../src/tool"

test("declares tools without args as permissive object schemas", () => {
  const registry = buildToolRegistry([
    {
      tool: {
        echo: {
          description: "Echo",
          execute: async () => "unused",
        },
      },
    },
  ])

  expect(registry.infos).toEqual([
    {
      name: "echo",
      description: "Echo",
      inputSchema: { type: "object", properties: {}, required: [] },
    },
  ])
})

test("accepts plain JSON-Schema args objects verbatim", () => {
  const schema = {
    type: "object",
    properties: { name: { type: "string" } },
    required: ["name"],
  }
  const registry = buildToolRegistry([
    {
      tool: {
        greet: {
          description: "Greet a user",
          args: schema,
          execute: async () => "unused",
        },
      },
    },
  ])

  expect(registry.infos[0]).toEqual({
    name: "greet",
    description: "Greet a user",
    inputSchema: schema,
  })
})

test("wraps non-object args in an object schema", () => {
  const registry = buildToolRegistry([
    {
      tool: {
        weird: {
          description: "Weird",
          args: "not-a-schema",
          execute: async () => "unused",
        },
      },
    },
  ])

  expect(registry.infos[0]?.inputSchema).toEqual({ type: "object" })
})

test("executes string tool results", async () => {
  const registry = buildToolRegistry([
    {
      tool: {
        echo: {
          description: "Echo",
          execute: async () => "plain text",
        },
      },
    },
  ])

  const reply = await callRegisteredTool(
    registry.tools,
    { tool: "echo", session: "s", call: "c", input: {} },
  )

  expect(reply.ok).toBe(true)
  expect(reply.output).toEqual({ title: "", output: "plain text", metadata: {} })
})

test("returns tool errors without throwing out of the adapter", async () => {
  const registry = buildToolRegistry([
    {
      tool: {
        fail: {
          description: "Fail",
          execute: async () => {
            throw new Error("boom")
          },
        },
      },
    },
  ])

  const reply = await callRegisteredTool(
    registry.tools,
    { tool: "fail", session: "s", call: "c", input: {} },
  )

  expect(reply.ok).toBe(false)
  expect(reply.output).toBe("boom")
})

test("tool context exposes only real session identity and output metadata", async () => {
  const registry = buildToolRegistry([{ tool: { inspect: {
    description: "Inspect context",
    execute: (_args: unknown, context: ToolContext) => {
      expect(Object.keys(context).sort()).toEqual(["metadata", "sessionID"])
      expect(context.sessionID).toBe("real-session")
      context.metadata({ title: "Real title", metadata: { checked: true } })
      return "done"
    },
  } } }])
  const reply = await callRegisteredTool(registry.tools, { tool: "inspect", session: "real-session", call: "c", input: {} })
  expect(reply.output).toEqual({ title: "Real title", output: "done", metadata: { checked: true } })
})

test("reports duplicate and malformed tool declarations", () => {
  const registry = buildToolRegistry([
    {
      tool: {
        echo: {
          description: "Echo",
          execute: async () => "first",
        },
      },
    },
    {
      tool: {
        echo: {
          description: "Echo again",
          execute: async () => "second",
        },
        broken: { description: 42, execute: async () => "unused" },
      },
    },
  ])

  expect(registry.errors).toHaveLength(2)
  expect(registry.errors[0]).toMatchObject({ kind: "duplicate", name: "echo" })
  expect(registry.errors[1]).toMatchObject({ kind: "malformed", name: "broken" })
})
