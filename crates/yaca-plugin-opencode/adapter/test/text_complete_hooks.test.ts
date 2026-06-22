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

const InitializeResultSchema = z.object({
  protocol_version: z.literal(1),
  plugin: z.object({
    id: z.literal("opencode"),
    version: z.string(),
    kind: z.literal("opencode"),
  }),
  hooks: z.array(z.object({ name: z.string() })),
  tools: z.array(z.unknown()),
})

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("experimental.text.complete replaces yaca assistant text", async () => {
  // Given: an OpenCode plugin mutates output.text for completed assistant text.
  const root = await makeTempDir()
  const pluginFile = path.join(root, "text-complete.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "text-complete",',
      "  server: async () => ({",
      '    "experimental.text.complete": async (input, output) => {',
      '      output.text = `${output.text} (${input.messageID}:${input.partID})`',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  // When: yaca dispatches the experimental.text.complete hook method.
  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(1),
    {
      jsonrpc: "2.0",
      id: 2,
      method: "hook/experimental.text.complete",
      params: {
        session: "session-1",
        message: "message-1",
        part: "part-1",
        text: "draft",
      },
    },
    shutdownRequest(3),
  ])

  // Then: the adapter registers the hook and returns the mutated text.
  const initialized = InitializeResultSchema.parse(responses[0]?.result)
  expect(initialized.hooks).toEqual([{ name: "experimental.text.complete" }])
  expect(responses[1]?.result).toEqual({
    outcome: "continue",
    text: "draft (message-1:part-1)",
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
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-text-complete-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}
