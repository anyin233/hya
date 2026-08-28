import { afterEach, expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { z } from "zod"

const AdapterResponseSchema = z.object({
  jsonrpc: z.literal("2.0"),
  id: z.number().int(),
  result: z.unknown().optional(),
  error: z
    .object({
      code: z.number().int(),
      message: z.string(),
    })
    .optional(),
})

const InitializeResultSchema = z.object({
  protocol_version: z.literal(1),
  plugin: z.object({
    id: z.literal("compat"),
    version: z.string(),
    kind: z.literal("compat"),
  }),
  hooks: z.array(z.object({ name: z.string() })),
  tools: z.array(
    z.object({
      name: z.string(),
      description: z.string(),
      inputSchema: z.unknown(),
    }),
  ),
  skills: z.array(
    z.object({
      id: z.string(),
      content: z.string(),
      digest: z.string(),
    }),
  ),
})
const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

async function runAdapter(
  requests: readonly unknown[],
  env?: Readonly<Record<string, string>>,
  argv: readonly string[] = [],
): Promise<readonly z.infer<typeof AdapterResponseSchema>[]> {
  const runRoot = await makeTempDir()
  const scriptArgs = argv.length === 0 ? [] : ["--", ...argv]
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts", ...scriptArgs], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    env: {
      ...process.env,
      HOME: runRoot,
      XDG_CONFIG_HOME: path.join(runRoot, "xdg"),
      HYA_DIRECTORY: runRoot,
      HYA_WORKTREE: runRoot,
      COMPAT_DISABLE_PROJECT_CONFIG: "1",
      ...env,
    },
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const stdin = proc.stdin
  if (stdin === undefined) {
    throw new Error("adapter stdin pipe was not created")
  }
  for (const request of requests) {
    stdin.write(`${JSON.stringify(request)}\n`)
  }
  await stdin.flush()
  stdin.end()

  const [stdout, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    proc.exited,
  ])
  expect(exitCode).toBe(0)
  return stdout
    .trim()
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => {
      const value: unknown = JSON.parse(line)
      return AdapterResponseSchema.parse(value)
    })
}

test("initialize returns hya compat plugin identity", async () => {
  const responses = await runAdapter([
    {
      jsonrpc: "2.0",
      id: 11,
      method: "initialize",
      params: { protocol_version: 1, host: { name: "hya", version: "0.0.0" } },
    },
    { jsonrpc: "2.0", id: 12, method: "shutdown", params: {} },
  ])

  expect(responses).toHaveLength(2)
  const first = responses[0]
  expect(first?.id).toBe(11)
  const result = InitializeResultSchema.parse(first?.result)
  expect(result.plugin.kind).toBe("compat")
  expect(result.hooks).toEqual([])
  expect(result.tools).toEqual([])
})

test("initialize publishes Skill contributions with exact fields", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "skills-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "skills",',
      "  server: async () => ({",
      '    skills: [{ id: "reviewer", content: "test", digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" }],',
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(
    [
      {
        jsonrpc: "2.0",
        id: 15,
        method: "initialize",
        params: { protocol_version: 1, host: { name: "hya", version: "0.0.0" } },
      },
      { jsonrpc: "2.0", id: 16, method: "shutdown", params: {} },
    ],
    {
      HYA_COMPAT_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      HYA_DIRECTORY: root,
      HYA_WORKTREE: root,
    },
  )

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.skills).toEqual([
    { id: "reviewer", content: "test", digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" },
  ])
})

test("initialize rejects duplicate Skill contributions", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "duplicate-skills-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "duplicate-skills",',
      "  server: async () => ({",
      '    skills: [\n      { id: "reviewer", content: "test", digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" },\n      { id: "reviewer", content: "test", digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" },\n    ],',
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(
    [
      {
        jsonrpc: "2.0",
        id: 17,
        method: "initialize",
        params: { protocol_version: 1, host: { name: "hya", version: "0.0.0" } },
      },
    ],
    {
      HYA_COMPAT_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      HYA_DIRECTORY: root,
      HYA_WORKTREE: root,
    },
  )

  expect(responses[0]?.error?.code).toBe(-32603)
  expect(responses[0]?.error?.message).toContain("duplicate Skill contribution id")
})

test("bundle activation initialize accepts exact operational metadata", async () => {
  const responses = await runAdapter([
    {
      jsonrpc: "2.0",
      id: 13,
      method: "initialize",
      params: {
        protocol_version: 1,
        host: { name: "hya", version: "0.34.11-test" },
        activation_id: "activation-test",
        lifecycle: "resident",
      },
    },
    { jsonrpc: "2.0", id: 14, method: "shutdown", params: {} },
  ])

  expect(responses).toHaveLength(2)
  const first = responses[0]
  expect(first?.id).toBe(13)
  expect(first?.error).toBeUndefined()
  const result = InitializeResultSchema.parse(first?.result)
  expect(result.plugin.id).toBe("compat")
  expect(result.plugin.kind).toBe("compat")
})

test("bundle activation loads only explicit materialized extension", async () => {
  const root = await makeTempDir()
  const extensionFile = path.join(root, "bundle-extension.ts")
  const normalPluginFile = path.join(root, "normal-plugin.ts")
  await writeFile(
    extensionFile,
    [
      "export default {",
      '  id: "bundle-extension",',
      "  server: async (input) => {",
      '    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {',
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
  await writeFile(
    normalPluginFile,
    [
      "export default {",
      '  id: "normal-plugin",',
      "  server: async () => ({",
      "    tool: {",
      "      leak: {",
      '        description: "Normal plugin leak",',
      '        execute: async () => "normal-leak",',
      "      },",
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(
    [
      {
        jsonrpc: "2.0",
        id: 51,
        method: "initialize",
        params: {
          protocol_version: 1,
          host: { name: "hya", version: "0.34.11-test" },
          activation_id: "activation-bundle-test",
          lifecycle: "transient",
        },
      },
      {
        jsonrpc: "2.0",
        id: 52,
        method: "tool/call",
        params: {
          tool: "echo",
          session: "session-bundle-test",
          call: "call-bundle-test",
          input: {},
        },
      },
      { jsonrpc: "2.0", id: 53, method: "shutdown", params: {} },
    ],
    {
      HYA_COMPAT_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(normalPluginFile).href],
      }),
      HYA_DIRECTORY: root,
      HYA_WORKTREE: root,
      HOME: root,
      XDG_CONFIG_HOME: path.join(root, "xdg"),
    },
    ["--bundle-extension", extensionFile],
  )

  expect(responses).toHaveLength(3)
  const initialized = responses[0]
  expect(initialized?.id).toBe(51)
  expect(initialized?.error).toBeUndefined()
  const result = InitializeResultSchema.parse(initialized?.result)
  expect(result.tools).toEqual([
    {
      name: "echo",
      description: "Bundle echo",
      inputSchema: { type: "object", properties: {}, required: [] },
    },
  ])
  expect(responses[1]?.id).toBe(52)
  expect(responses[1]?.error).toBeUndefined()
  expect(responses[1]?.result).toMatchObject({
    ok: true,
    output: { output: "bundle-echo" },
  })
})

test("initialize declares hooks from configured local plugins", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "hooks",',
      "  server: async () => ({",
      "    event: async () => {},",
      '    "tool.execute.before": async () => {},',
      '    "chat.params": async () => {},',
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(
    [
      {
        jsonrpc: "2.0",
        id: 31,
        method: "initialize",
        params: { protocol_version: 1, host: { name: "hya", version: "0.0.0" } },
      },
      { jsonrpc: "2.0", id: 32, method: "shutdown", params: {} },
    ],
    {
      HYA_COMPAT_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      HYA_DIRECTORY: root,
      HYA_WORKTREE: root,
    },
  )

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.hooks).toEqual([
    { name: "event" },
    { name: "chat.params" },
    { name: "tool.execute.before" },
  ])
})

test("initialize declares Compat tools and tool calls execute them", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "tool-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "tools",',
      "  server: async () => ({",
      "    tool: {",
      "      greet: {",
      '        description: "Greet a user",',
      '        args: { name: { type: "string" } },',
      "        execute: async (args, ctx) => {",
      '          ctx.metadata({ title: "Greeting", metadata: { via: "ctx" } })',
      '          return { output: `hi ${args.name}`, metadata: { direct: true } }',
      "        },",
      "      },",
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(
    [
      {
        jsonrpc: "2.0",
        id: 41,
        method: "initialize",
        params: { protocol_version: 1, host: { name: "hya", version: "0.0.0" } },
      },
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
      { jsonrpc: "2.0", id: 43, method: "shutdown", params: {} },
    ],
    {
      HYA_COMPAT_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      HYA_DIRECTORY: root,
      HYA_WORKTREE: root,
    },
  )

  const initialized = InitializeResultSchema.parse(responses[0]?.result)
  expect(initialized.tools).toEqual([
    {
      name: "greet",
      description: "Greet a user",
      inputSchema: {
        type: "object",
        properties: { name: { type: "string" } },
        required: ["name"],
      },
    },
  ])
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
  const responses = await runAdapter([
    { jsonrpc: "2.0", id: 21, method: "missing", params: {} },
    { jsonrpc: "2.0", id: 22, method: "shutdown", params: {} },
  ])

  expect(responses).toHaveLength(2)
  expect(responses[0]?.id).toBe(21)
  expect(responses[0]?.error?.code).toBe(-32601)
  expect(responses[1]?.id).toBe(22)
  expect(responses[1]?.result).toEqual({})
})

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "hya-compat-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}
