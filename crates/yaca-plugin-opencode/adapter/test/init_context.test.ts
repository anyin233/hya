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
})

const InitializeResultSchema = z.object({
  hooks: z.array(z.object({ name: z.string() })),
})

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("initialize passes OpenCode shell and project time to local plugins", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "input-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "input",',
      "  server: async (input) => {",
      '    if (typeof input.$ !== "function") throw new Error("missing shell")',
      '    if (typeof input.project.time.created !== "number") throw new Error("missing project time")',
      "    return { event: async () => {} }",
      "  },",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile)

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.hooks).toEqual([{ name: "event" }])
})

async function runAdapter(
  root: string,
  pluginFile: string,
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
  stdin.write(
    `${JSON.stringify({
      jsonrpc: "2.0",
      id: 51,
      method: "initialize",
      params: { protocol_version: 1, host: { name: "yaca", version: "0.0.0" } },
    })}\n`,
  )
  stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 52, method: "shutdown", params: {} })}\n`)
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

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}
