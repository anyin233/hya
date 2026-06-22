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
    id: z.literal("opencode"),
    version: z.string(),
    kind: z.literal("opencode"),
  }),
  hooks: z.array(z.object({ name: z.string() })),
  tools: z.array(
    z.object({
      name: z.string(),
      description: z.string(),
      inputSchema: z.unknown(),
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
): Promise<readonly z.infer<typeof AdapterResponseSchema>[]> {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts"], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    env: { ...process.env, ...env },
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

test("initialize returns yaca opencode plugin identity", async () => {
  const responses = await runAdapter([
    {
      jsonrpc: "2.0",
      id: 11,
      method: "initialize",
      params: { protocol_version: 1, host: { name: "yaca", version: "0.0.0" } },
    },
    { jsonrpc: "2.0", id: 12, method: "shutdown", params: {} },
  ])

  expect(responses).toHaveLength(2)
  const first = responses[0]
  expect(first?.id).toBe(11)
  const result = InitializeResultSchema.parse(first?.result)
  expect(result.plugin.kind).toBe("opencode")
  expect(result.hooks).toEqual([])
  expect(result.tools).toEqual([])
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
        params: { protocol_version: 1, host: { name: "yaca", version: "0.0.0" } },
      },
      { jsonrpc: "2.0", id: 32, method: "shutdown", params: {} },
    ],
    {
      YACA_OPENCODE_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      YACA_DIRECTORY: root,
      YACA_WORKTREE: root,
    },
  )

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.hooks).toEqual([
    { name: "event" },
    { name: "chat.params" },
    { name: "tool.execute.before" },
  ])
})

test("initialize declares OpenCode tools and tool calls execute them", async () => {
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
        params: { protocol_version: 1, host: { name: "yaca", version: "0.0.0" } },
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
      YACA_OPENCODE_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      YACA_DIRECTORY: root,
      YACA_WORKTREE: root,
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
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}
