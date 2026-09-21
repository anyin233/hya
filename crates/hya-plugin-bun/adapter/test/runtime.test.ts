import { afterEach, expect, test } from "bun:test"
import { writeFile } from "node:fs/promises"
import path from "node:path"
import { pathToFileURL } from "node:url"

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

async function writeExtension(root: string, name: string, body: string): Promise<string> {
  const file = path.join(root, name)
  await writeFile(file, body)
  return file
}

test("initialize returns hya bun plugin identity", async () => {
  const responses = await runAdapterProcess([
    initializeRequest(11),
    shutdownRequest(12),
  ])

  expect(responses).toHaveLength(2)
  const first = responses[0]
  expect(first?.id).toBe(11)
  expect(first?.result).toEqual({
    protocol_version: 1,
    plugin: { id: "bun", version: "1.0.0", kind: "bun" },
    hooks: [],
    tools: [],
    skills: [],
    workspaceAdapters: [],
  })
})

test("bundle activation accepts exact operational metadata", async () => {
  const responses = await runAdapterProcess([
    initializeRequest(13, {
      activation_id: "activation-test",
      lifecycle: "resident",
    }),
    shutdownRequest(14),
  ])

  expect(responses).toHaveLength(2)
  expect(responses[0]?.id).toBe(13)
  expect(responses[0]?.error).toBeUndefined()
  const result = responses[0]?.result as { plugin: { kind: string } }
  expect(result.plugin.kind).toBe("bun")
})

test("initialize rejects mismatched activation metadata", async () => {
  const responses = await runAdapterProcess([
    initializeRequest(15, { activation_id: "lonely" }),
    shutdownRequest(16),
  ])

  expect(responses[0]?.error?.code).toBe(-32602)
  expect(responses[0]?.error?.message).toContain(
    "activation_id and lifecycle must be supplied together",
  )
})

test("bundle activation loads only explicit materialized extension", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "bundle-extension.ts",
    [
      "export default {",
      '  id: "bundle-extension",',
      "  server: async (context) => {",
      "    if (context === null || typeof context !== \"object\" || Object.keys(context).length !== 0) {",
      '      throw new Error("bundle extension received unexpected initialization input")',
      "    }",
      "    return {",
      "      tool: {",
      "        echo: {",
      '          description: "Bundle echo",',
      '          execute: async () => "bundle-echo",',
      "        },",
      "      },",
      "    }",
      "  },",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(51, {
        activation_id: "activation-bundle-test",
        lifecycle: "transient",
      }),
      {
        jsonrpc: "2.0",
        id: 52,
        method: "tool/call",
        params: {
          tool: "echo",
          session: "session-bundle-test",
          call: "call-bundle-test",
          input: { value: 1 },
        },
      },
      shutdownRequest(53),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses).toHaveLength(3)
  const initialized = responses[0]
  expect(initialized?.id).toBe(51)
  expect(initialized?.error).toBeUndefined()
  expect(initialized?.result).toMatchObject({
    plugin: { id: "bun", kind: "bun" },
    tools: [
      {
        name: "echo",
        description: "Bundle echo",
        inputSchema: { type: "object", properties: {}, required: [] },
      },
    ],
  })
  expect(responses[1]?.id).toBe(52)
  expect(responses[1]?.error).toBeUndefined()
  expect(responses[1]?.result).toMatchObject({
    ok: true,
    output: { output: "bundle-echo" },
  })
})

test("--extension is an alias for --bundle-extension", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "alias-extension.ts",
    [
      "export default {",
      '  id: "alias-extension",',
      "  server: async () => ({",
      "    tool: {",
      "      ping: {",
      '        description: "Ping",',
      '        execute: async () => "pong",',
      "      },",
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(61, {
        activation_id: "activation-alias-test",
        lifecycle: "transient",
      }),
      shutdownRequest(62),
    ],
    { argv: ["--extension", extensionFile] },
  )

  expect(responses[0]?.result).toMatchObject({
    tools: [{ name: "ping", description: "Ping" }],
  })
})

test("extension factories always receive one frozen empty object", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "context-extension.ts",
    [
      "export default {",
      '  id: "context-extension",',
      "  server: async (context) => {",
      '    if (context === undefined || context === null) throw new Error("missing context")',
      "    if (Object.keys(context).length !== 0) {",
      '      throw new Error("context must be empty")',
      "    }",
      "    if (!Object.isFrozen(context)) {",
      '      throw new Error("context must be frozen")',
      "    }",
      "    return { tool: {",
      "      where: {",
      '        description: "context-ok",',
      '        execute: async () => "unused",',
      "      },",
      "    } }",
      "  },",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(71, {
        activation_id: "activation-context-test",
        lifecycle: "transient",
      }),
      shutdownRequest(72),
    ],
    {
      argv: ["--bundle-extension", extensionFile],
      env: { HYA_BUNDLE_CONFIG_DIR: root },
    },
  )

  expect(responses[0]?.result).toMatchObject({
    tools: [{ name: "where", description: "context-ok" }],
  })
})

test("initialize declares hya wire hooks from loaded extensions", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "hooks-extension.ts",
    [
      "export default {",
      '  id: "hooks",',
      "  server: async () => ({",
      "    event: async () => {},",
      '    "tool.execute.before": async () => {},',
      '    "permission.ask": async () => "defer",',
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(31, {
        activation_id: "activation-hooks-test",
        lifecycle: "transient",
      }),
      shutdownRequest(32),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses[0]?.result).toMatchObject({
    hooks: [
      { name: "event" },
      { name: "tool.execute.before" },
      { name: "permission.ask" },
    ],
  })
})

test("initialize publishes Skill contributions with exact fields", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "skills-extension.ts",
    [
      "export default {",
      '  id: "skills",',
      "  server: async () => ({",
      '    skills: [{ id: "reviewer", content: "test", digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" }],',
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(81, {
        activation_id: "activation-skills-test",
        lifecycle: "transient",
      }),
      shutdownRequest(82),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses[0]?.result).toMatchObject({
    skills: [
      {
        id: "reviewer",
        content: "test",
        digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
      },
    ],
  })
})

test("initialize rejects mismatched Skill digests", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "bad-digest-extension.ts",
    [
      "export default {",
      '  id: "bad-digest",',
      "  server: async () => ({",
      '    skills: [{ id: "reviewer", content: "test", digest: "0000000000000000000000000000000000000000000000000000000000000000" }],',
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(83, {
        activation_id: "activation-bad-digest-test",
        lifecycle: "transient",
      }),
      shutdownRequest(84),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses[0]?.error?.code).toBe(-32603)
  expect(responses[0]?.error?.message).toContain("digest does not match")
})

test("initialize rejects load failures from explicit extensions", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "broken-extension.ts",
    "throw new Error('broken on import')",
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(85, {
        activation_id: "activation-broken-test",
        lifecycle: "transient",
      }),
      shutdownRequest(86),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses[0]?.error?.code).toBe(-32603)
  expect(responses[0]?.error?.message).toContain("broken on import")
})

test("initialize declares tools with declared JSON-Schema args and executes them", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "tool-extension.ts",
    [
      "export default {",
      '  id: "tools",',
      "  server: async () => ({",
      "    tool: {",
      "      greet: {",
      '        description: "Greet a user",',
      '        args: { type: "object", properties: { name: { type: "string" } }, required: ["name"] },',
      "        execute: async (args, ctx) => {",
      '          ctx.metadata({ title: "Greeting", metadata: { via: "ctx" } })',
      "          return { output: `hi ${args.name}`, metadata: { direct: true } }",
      "        },",
      "      },",
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(41, {
        activation_id: "activation-tool-test",
        lifecycle: "transient",
      }),
      {
        jsonrpc: "2.0",
        id: 42,
        method: "tool/call",
        params: {
          tool: "greet",
          session: "session-1",
          call: "call-1",
          input: { name: "Ada" },
        },
      },
      shutdownRequest(43),
    ],
    { argv: ["--extension", extensionFile] },
  )

  expect(responses[0]?.result).toMatchObject({
    tools: [
      {
        name: "greet",
        description: "Greet a user",
        inputSchema: {
          type: "object",
          properties: { name: { type: "string" } },
          required: ["name"],
        },
      },
    ],
  })
  expect(responses[1]?.result).toMatchObject({
    ok: true,
    output: {
      title: "Greeting",
      output: "hi Ada",
      metadata: { via: "ctx", direct: true },
    },
  })
})

test("unknown methods return JSON-RPC method-not-found errors", async () => {
  const responses = await runAdapterProcess([
    { jsonrpc: "2.0", id: 21, method: "missing", params: {} },
    shutdownRequest(22),
  ])

  expect(responses).toHaveLength(2)
  expect(responses[0]?.id).toBe(21)
  expect(responses[0]?.error?.code).toBe(-32601)
  expect(responses[1]?.result).toEqual({})
})

test("file:// URLs are accepted as extension flags", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "url-extension.ts",
    "export default { id: 'url', server: async () => ({}) }",
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(91, {
        activation_id: "activation-url-test",
        lifecycle: "transient",
      }),
      shutdownRequest(92),
    ],
    { argv: ["--bundle-extension", pathToFileURL(extensionFile).href] },
  )

  expect(responses[0]?.error).toBeUndefined()
})

test("initialize publishes validated workspace adapter declarations", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "workspace-extension.ts",
    [
      "export default {",
      '  id: "workspace",',
      "  server: async () => ({",
      "    workspaceAdapters: [",
      '      { type: "example", name: "Example adapter", description: "Example" },',
      "    ],",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(93, {
        activation_id: "activation-workspace-test",
        lifecycle: "transient",
      }),
      shutdownRequest(94),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses[0]?.result).toMatchObject({
    workspaceAdapters: [
      { type: "example", name: "Example adapter", description: "Example" },
    ],
  })
})

test("initialize rejects malformed workspace adapter declarations", async () => {
  const root = await makeTempDir()
  const extensionFile = await writeExtension(
    root,
    "bad-workspace-extension.ts",
    [
      "export default {",
      '  id: "bad-workspace",',
      "  server: async () => ({",
      "    workspaceAdapters: [{ type: \"\", name: \"Nope\", description: \"x\" }],",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(95, {
        activation_id: "activation-bad-workspace-test",
        lifecycle: "transient",
      }),
      shutdownRequest(96),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses[0]?.error?.code).toBe(-32603)
  expect(responses[0]?.error?.message).toContain("workspaceAdapters")
})
