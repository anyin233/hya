import { afterEach, expect, test } from "bun:test"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { z } from "zod"

const AdapterResponseSchema = z.object({
  jsonrpc: z.literal("2.0"),
  id: z.number().int(),
  result: z.unknown().optional(),
})

const InitializeResultSchema = z.object({
  hooks: z.array(z.object({ name: z.string() })),
  tools: z.array(z.object({ name: z.string() })),
})

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("OPENCODE_PURE skips configured external plugins", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "pure-test",',
      "  server: async () => ({",
      "    event: async () => {},",
      "    tool: {",
      "      greet: {",
      '        description: "Greet",',
      "        args: {},",
      '        execute: async () => "hi",',
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
        id: 1,
        method: "initialize",
        params: { protocol_version: 1, host: { name: "yaca", version: "0.0.0" } },
      },
      { jsonrpc: "2.0", id: 2, method: "shutdown", params: {} },
    ],
    {
      OPENCODE_PURE: "1",
      YACA_OPENCODE_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      YACA_DIRECTORY: root,
      YACA_WORKTREE: root,
    },
  )

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.hooks).toEqual([])
  expect(result.tools).toEqual([])
})

async function runAdapter(
  requests: readonly unknown[],
  env: Readonly<Record<string, string>>,
): Promise<readonly z.infer<typeof AdapterResponseSchema>[]> {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts"], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    env: { ...process.env, ...env },
    stdin: "pipe",
    stdout: "pipe",
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
    .map((line) => AdapterResponseSchema.parse(JSON.parse(line)))
}

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  tempDirs.push(created)
  return created
}
