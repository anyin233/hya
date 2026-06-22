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

test("experimental.chat.messages.transform rewrites yaca chat messages during chat params", async () => {
  // Given: an OpenCode plugin that mutates the first text part in message history.
  const root = await makeTempDir()
  const pluginFile = path.join(root, "chat-messages-transform.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "chat-messages-transform",',
      "  server: async () => ({",
      '    "experimental.chat.messages.transform": async (_input, output) => {',
      '      if (output.messages[0].info.role !== "user") throw new Error("bad role")',
      '      if (output.messages[0].parts[0].type !== "text") throw new Error("bad part")',
      '      output.messages[0].parts[0].text = "rewritten by plugin"',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  // When: yaca asks the adapter to run chat.params for the same request history.
  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(21),
    {
      jsonrpc: "2.0",
      id: 22,
      method: "hook/chat.params",
      params: {
        session: "session-3",
        message: "message-3",
        request: {
          model: "openai/gpt-5",
          messages: [
            {
              role: "user",
              id: "message-3",
              parts: [{ type: "text", id: "part-3", text: "original" }],
            },
          ],
          tools: [],
        },
      },
    },
    shutdownRequest(23),
  ])

  // Then: the OpenCode transform hook is advertised and its text mutation is preserved.
  expect(responses[0]?.result).toMatchObject({
    hooks: [{ name: "chat.params" }],
  })
  expect(responses[1]?.result).toEqual({
    outcome: "continue",
    request: {
      model: "openai/gpt-5",
      messages: [
        {
          role: "user",
          id: "message-3",
          parts: [{ type: "text", id: "part-3", text: "rewritten by plugin" }],
        },
      ],
      tools: [],
      temperature: 0,
    },
  })
})

test("experimental.chat.messages.transform preserves assistant metadata when rewriting text", async () => {
  // Given: an assistant message using yaca metadata that differs from OpenCode metadata.
  const root = await makeTempDir()
  const pluginFile = path.join(root, "assistant-messages-transform.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "assistant-messages-transform",',
      "  server: async () => ({",
      '    "experimental.chat.messages.transform": async (_input, output) => {',
      '      if (output.messages[0].info.role !== "assistant") throw new Error("bad role")',
      '      if (output.messages[0].parts[0].type !== "text") throw new Error("bad part")',
      '      output.messages[0].parts[0].text = "assistant rewrite"',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  // When: the transform hook rewrites only the assistant text part.
  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(31),
    {
      jsonrpc: "2.0",
      id: 32,
      method: "hook/chat.params",
      params: {
        session: "session-4",
        message: "message-4",
        request: {
          model: "gpt-5",
          messages: [
            {
              role: "assistant",
              id: "assistant-1",
              agent: "coder",
              model: "gpt-5",
              parts: [{ type: "text", id: "assistant-part-1", text: "assistant original" }],
              finish: "stop",
            },
          ],
          tools: [],
        },
      },
    },
    shutdownRequest(33),
  ])

  // Then: yaca assistant metadata is preserved while the text mutation is applied.
  expect(responses[1]?.result).toEqual({
    outcome: "continue",
    request: {
      model: "gpt-5",
      messages: [
        {
          role: "assistant",
          id: "assistant-1",
          agent: "coder",
          model: "gpt-5",
          parts: [{ type: "text", id: "assistant-part-1", text: "assistant rewrite" }],
          finish: "stop",
        },
      ],
      tools: [],
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
      YACA_OPENCODE_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      YACA_DIRECTORY: root,
      YACA_WORKTREE: root,
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
    params: { protocol_version: 1, host: { name: "yaca", version: "0.0.0" } },
  }
}

function shutdownRequest(id: number): unknown {
  return { jsonrpc: "2.0", id, method: "shutdown", params: {} }
}

async function makeTempDir(): Promise<string> {
  const root = await mkdtemp(path.join(tmpdir(), "yaca-opencode-chat-messages-"))
  tempDirs.push(root)
  await mkdir(path.join(root, ".opencode", "plugins"), { recursive: true })
  return root
}
