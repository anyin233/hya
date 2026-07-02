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

test("chat.headers adds Compat plugin headers during chat params", async () => {
  // Given: an Compat plugin that derives request headers from chat context.
  const root = await makeTempDir()
  const pluginFile = path.join(root, "chat-headers.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "chat-headers",',
      "  server: async () => ({",
      '    "chat.headers": async (input, output) => {',
      '      if (input.sessionID !== "session-headers") throw new Error("bad session")',
      '      if (input.message.id !== "message-headers") throw new Error("bad message")',
      '      if (input.model.id !== "gpt-5") throw new Error("bad model")',
      '      output.headers["x-hya-session"] = input.sessionID',
      '      output.headers["x-hya-model"] = input.model.id',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  // When: hya asks the adapter to prepare chat parameters.
  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(41),
    {
      jsonrpc: "2.0",
      id: 42,
      method: "hook/chat.params",
      params: {
        session: "session-headers",
        message: "message-headers",
        request: {
          model: "openai/gpt-5",
          messages: [],
          tools: [],
        },
      },
    },
    shutdownRequest(43),
  ])

  // Then: the Compat chat.headers hook is exposed through hya chat.params.
  expect(responses[0]?.result).toMatchObject({
    hooks: [{ name: "chat.params" }],
  })
  expect(responses[1]?.result).toEqual({
    outcome: "continue",
    request: {
      model: "openai/gpt-5",
      messages: [],
      tools: [],
      temperature: 0,
      headers: {
        "x-hya-session": "session-headers",
        "x-hya-model": "gpt-5",
      },
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
      HYA_COMPAT_OPTIONS_JSON: JSON.stringify({
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
  const root = await mkdtemp(path.join(tmpdir(), "hya-compat-chat-headers-"))
  tempDirs.push(root)
  await mkdir(path.join(root, ".opencode", "plugins"), { recursive: true })
  return root
}
