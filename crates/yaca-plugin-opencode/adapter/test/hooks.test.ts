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

test("tool.execute.before mutates yaca tool input", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "before.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "before",',
      "  server: async () => ({",
      '    "tool.execute.before": async (_input, output) => {',
      '      output.args.command = `${output.args.command} --safe`',
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
      method: "hook/tool.execute.before",
      params: {
        session: "session-1",
        message: "message-1",
        call: "call-1",
        tool: "shell",
        input: { command: "ls" },
      },
    },
    shutdownRequest(3),
  ])

  expect(responses[1]?.result).toEqual({
    outcome: "continue",
    input: { command: "ls --safe" },
  })
})

test("tool.execute.before throw becomes yaca veto", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "veto.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "veto",',
      "  server: async () => ({",
      '    "tool.execute.before": async () => { throw new Error("blocked") },',
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(11),
    {
      jsonrpc: "2.0",
      id: 12,
      method: "hook/tool.execute.before",
      params: {
        session: "session-1",
        message: "message-1",
        call: "call-1",
        tool: "shell",
        input: { command: "rm -rf /tmp/nope" },
      },
    },
    shutdownRequest(13),
  ])

  expect(responses[1]?.result).toEqual({
    outcome: "veto",
    reason: "blocked",
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
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}
