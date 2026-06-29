import { afterEach, expect, test } from "bun:test"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { z } from "zod"

const AdapterResponseSchema = z.object({
  jsonrpc: z.literal("2.0"),
  id: z.number().int(),
  result: z.unknown().optional(),
})

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("shutdown runs OpenCode dispose hooks", async () => {
  const root = await makeTempDir()
  const marker = path.join(root, "disposed.txt")
  const pluginFile = path.join(root, "plugin.ts")
  await writeFile(
    pluginFile,
    [
      'import { writeFile } from "node:fs/promises"',
      "export default {",
      '  id: "dispose-test",',
      "  server: async () => ({",
      "    dispose: async () => {",
      '      await writeFile(process.env.HYA_DISPOSE_MARKER, "disposed")',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  await runAdapter(
    [
      {
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: { protocol_version: 1, host: { name: "hya", version: "0.0.0" } },
      },
      { jsonrpc: "2.0", id: 2, method: "shutdown", params: {} },
    ],
    {
      HYA_DISPOSE_MARKER: marker,
      HYA_OPENCODE_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      HYA_DIRECTORY: root,
      HYA_WORKTREE: root,
      XDG_CONFIG_HOME: path.join(root, "xdg"),
      HOME: path.join(root, "home"),
    },
  )

  await expect(readFile(marker, "utf8")).resolves.toBe("disposed")
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
  const created = await mkdtemp(path.join(tmpdir(), "hya-opencode-"))
  tempDirs.push(created)
  return created
}
