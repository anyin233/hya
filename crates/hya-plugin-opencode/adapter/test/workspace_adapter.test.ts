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
  workspaceAdapters: z.array(
    z.object({
      type: z.string(),
      name: z.string(),
      description: z.string(),
    }),
  ),
})

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("initialize reports workspace adapters registered by local plugins", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "workspace-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "workspace",',
      "  server: async ({ experimental_workspace }) => {",
      '    experimental_workspace.register("folder", {',
      '      name: "Folder",',
      '      description: "Local folder workspace",',
      "      configure(input) { return input },",
      "      async create() {},",
      "      async remove() {},",
      '      target(input) { return { type: "local", directory: input.directory } }',
      "    })",
      "    return {}",
      "  },",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile)

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.workspaceAdapters).toEqual([
    {
      type: "folder",
      name: "Folder",
      description: "Local folder workspace",
    },
  ])
})

async function runAdapter(
  root: string,
  pluginFile: string,
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
  stdin.write(
    `${JSON.stringify({
      jsonrpc: "2.0",
      id: 61,
      method: "initialize",
      params: { protocol_version: 1, host: { name: "hya", version: "0.0.0" } },
    })}\n`,
  )
  stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 62, method: "shutdown", params: {} })}\n`)
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
  const created = await mkdtemp(path.join(tmpdir(), "hya-opencode-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}
