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

test("permission.ask maps OpenCode statuses to hya outcomes", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "permission.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "permission",',
      "  server: async () => ({",
      '    "permission.ask": async (input, output) => {',
      '      if (input.sessionID !== "session-1") throw new Error("bad session")',
      '      if (input.type === "bash" && input.pattern === "git status") output.status = "allow"',
      '      else if (input.type === "edit") output.status = "deny"',
      '      else output.status = "ask"',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(1),
    permissionRequest(2, "bash", { type: "command", value: "git status" }),
    permissionRequest(3, "edit", { type: "path", value: "README.md" }),
    permissionRequest(4, "read", { type: "path", value: "README.md" }),
    shutdownRequest(5),
  ])

  expect(responses[1]?.result).toEqual({ outcome: "allow_once" })
  expect(responses[2]?.result).toEqual({ outcome: "reject" })
  expect(responses[3]?.result).toEqual({ outcome: "defer" })
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

function permissionRequest(id: number, action: string, resource: unknown): unknown {
  return {
    jsonrpc: "2.0",
    id,
    method: "hook/permission.ask",
    params: {
      session: "session-1",
      action,
      resource,
    },
  }
}

function shutdownRequest(id: number): unknown {
  return { jsonrpc: "2.0", id, method: "shutdown", params: {} }
}

async function makeTempDir(): Promise<string> {
  const root = await mkdtemp(path.join(tmpdir(), "hya-opencode-permission-"))
  tempDirs.push(root)
  await mkdir(path.join(root, ".opencode", "plugins"), { recursive: true })
  return root
}
