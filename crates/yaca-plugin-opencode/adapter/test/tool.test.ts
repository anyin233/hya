import { expect, test } from "bun:test"
import { z } from "zod"

import {
  buildToolRegistry,
  callRegisteredTool,
  type ToolContext,
} from "../src/tool"

test("converts OpenCode tool definitions to yaca tool infos", () => {
  const registry = buildToolRegistry([
    {
      tool: {
        greet: {
          description: "Greet a user",
          args: { name: z.string() },
          execute: async () => "unused",
        },
        legacy: {
          description: "Legacy schema",
          args: { count: { type: "number" } },
          execute: async () => "unused",
        },
      },
    },
  ])

  expect(registry.infos).toHaveLength(2)
  expect(registry.infos[0]).toMatchObject({
    name: "greet",
    description: "Greet a user",
    inputSchema: {
      type: "object",
      properties: { name: { type: "string" } },
      required: ["name"],
    },
  })
  expect(registry.infos[0]).not.toHaveProperty("input_schema")
  expect(registry.infos[1]).toEqual({
    name: "legacy",
    description: "Legacy schema",
    inputSchema: {
      type: "object",
      properties: { count: { type: "number" } },
      required: ["count"],
    },
  })
})

test("executes string tool results", async () => {
  const registry = buildToolRegistry([
    {
      tool: {
        echo: {
          description: "Echo",
          args: {},
          execute: async () => "plain text",
        },
      },
    },
  ])

  const reply = await callRegisteredTool(
    registry.tools,
    { tool: "echo", session: "s", call: "c", input: {} },
    { directory: "/tmp", worktree: "/tmp" },
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
          args: {},
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
    { directory: "/tmp", worktree: "/tmp" },
  )

  expect(reply.ok).toBe(false)
  expect(reply.output).toBe("boom")
})

test("surfaces unsupported context ask calls as tool errors", async () => {
  const registry = buildToolRegistry([
    {
      tool: {
        ask: {
          description: "Ask",
          args: {},
          execute: async (_args: unknown, context: ToolContext) => context.ask(),
        },
      },
    },
  ])

  const reply = await callRegisteredTool(
    registry.tools,
    { tool: "ask", session: "s", call: "c", input: {} },
    { directory: "/tmp", worktree: "/tmp" },
  )

  expect(reply.ok).toBe(false)
  expect(reply.output).toContain("ask() is not supported")
})
