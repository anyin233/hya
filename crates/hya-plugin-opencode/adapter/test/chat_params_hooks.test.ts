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
  error: z.unknown().optional(),
})

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("chat.params mutates supported hya request fields", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "chat-params.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "chat-params",',
      "  server: async () => ({",
      '    "experimental.chat.system.transform": async (input, output) => {',
      '      if (input.sessionID !== "session-1") throw new Error("bad session")',
      '      if (input.model.id !== "gpt-5") throw new Error("bad model")',
      '      output.system.push("plugin system")',
      "    },",
      '    "chat.params": async (input, output) => {',
      '      if (input.message.id !== "message-1") throw new Error("bad message")',
      "      output.temperature = 0.25",
      "      output.maxOutputTokens = 123",
      "      output.topP = 0.5",
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(1),
    {
      jsonrpc: "2.0",
      id: 2,
      method: "hook/chat.params",
      params: {
        session: "session-1",
        message: "message-1",
        request: {
          model: "openai/gpt-5",
          system: "base system",
          messages: [
            {
              role: "user",
              id: "message-1",
              parts: [{ type: "text", id: "part-1", text: "hello" }],
            },
          ],
          tools: [],
          temperature: 0.7,
          max_output_tokens: 456,
          reasoning: "high",
        },
      },
    },
    shutdownRequest(3),
  ])

  expect(responses[1]?.result).toEqual({
    outcome: "continue",
    request: {
      model: "openai/gpt-5",
      system: "base system\n\nplugin system",
      messages: [
        {
          role: "user",
          id: "message-1",
          parts: [{ type: "text", id: "part-1", text: "hello" }],
        },
      ],
      tools: [],
      temperature: 0.25,
      max_output_tokens: 123,
      reasoning: "high",
    },
  })
})

test("tool.definition mutates hya tool definitions during chat params", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "tool-definition.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "tool-definition",',
      "  server: async () => ({",
      '    "tool.definition": async (input, output) => {',
      '      if (input.toolID !== "shell") return',
      '      output.description = "Run a reviewed shell command"',
      '      output.parameters = { type: "object", properties: { command: { type: "string", minLength: 1 } }, required: ["command"] }',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(11),
    {
      jsonrpc: "2.0",
      id: 12,
      method: "hook/chat.params",
      params: {
        session: "session-2",
        message: "message-2",
        request: {
          model: "openai/gpt-5",
          messages: [],
          tools: [
            {
              name: "shell",
              description: "old shell",
              input_schema: { type: "object" },
              output_schema: { type: "object", properties: { output: { type: "string" } } },
            },
            {
              name: "read",
              description: "Read a file",
              input_schema: { type: "object", properties: { filePath: { type: "string" } } },
            },
          ],
        },
      },
    },
    shutdownRequest(13),
  ])

  expect(responses[0]?.result).toMatchObject({
    hooks: [{ name: "chat.params" }],
  })
  expect(responses[1]?.result).toEqual({
    outcome: "continue",
    request: {
      model: "openai/gpt-5",
      messages: [],
      tools: [
        {
          name: "shell",
          description: "Run a reviewed shell command",
          input_schema: {
            type: "object",
            properties: { command: { type: "string", minLength: 1 } },
            required: ["command"],
          },
          output_schema: { type: "object", properties: { output: { type: "string" } } },
        },
        {
          name: "read",
          description: "Read a file",
          input_schema: { type: "object", properties: { filePath: { type: "string" } } },
        },
      ],
      temperature: 0,
    },
  })
})

async function runAdapter(
  root: string,
  pluginFile: string,
  requests: readonly unknown[],
): Promise<readonly z.infer<typeof AdapterResponseSchema>[]> {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts"], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    env: {
      ...process.env,
      HYA_OPENCODE_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      HYA_DIRECTORY: root,
      HYA_WORKTREE: root,
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

function initializeRequest(id: number): unknown {
  return {
    jsonrpc: "2.0",
    id,
    method: "initialize",
    params: { protocol_version: 1, host: { name: "hya", version: "0.0.0" } },
  }
}

function shutdownRequest(id: number): unknown {
  return { jsonrpc: "2.0", id, method: "shutdown", params: {} }
}

async function makeTempDir(): Promise<string> {
  const root = await mkdtemp(path.join(tmpdir(), "hya-opencode-chat-"))
  tempDirs.push(root)
  await mkdir(path.join(root, ".opencode", "plugins"), { recursive: true })
  return root
}
