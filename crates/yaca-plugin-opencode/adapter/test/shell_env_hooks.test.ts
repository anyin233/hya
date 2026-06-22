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

test("shell.env adds OpenCode plugin env to shell tool input", async () => {
  // Given: an OpenCode plugin that adds a shell environment variable.
  const root = await makeTempDir()
  const expectedCwd = path.join(root, "subdir")
  const pluginFile = path.join(root, "shell-env.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "shell-env",',
      "  server: async () => ({",
      '    "shell.env": async (input, output) => {',
      `      if (input.cwd !== ${JSON.stringify(expectedCwd)}) throw new Error("bad cwd")`,
      '      if (input.sessionID !== "session-shell") throw new Error("bad session")',
      '      if (input.callID !== "call-shell") throw new Error("bad call")',
      '      output.env.YACA_SHELL_ENV = "from-plugin"',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  // When: yaca asks the adapter to prepare a shell tool execution.
  const responses = await runAdapter(root, pluginFile, [
    initializeRequest(51),
    {
      jsonrpc: "2.0",
      id: 52,
      method: "hook/tool.execute.before",
      params: {
        session: "session-shell",
        message: "message-shell",
        call: "call-shell",
        tool: "shell",
        input: {
          command: "printf %s \"$YACA_SHELL_ENV\"",
          workdir: "subdir",
          env: { EXISTING: "kept" },
        },
      },
    },
    shutdownRequest(53),
  ])

  // Then: shell.env is exposed through tool.execute.before and merged into args.
  expect(responses[0]?.result).toMatchObject({
    hooks: [{ name: "tool.execute.before" }],
  })
  expect(responses[1]?.result).toEqual({
    outcome: "continue",
    input: {
      command: "printf %s \"$YACA_SHELL_ENV\"",
      workdir: "subdir",
      env: {
        EXISTING: "kept",
        YACA_SHELL_ENV: "from-plugin",
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
  const root = await mkdtemp(path.join(tmpdir(), "yaca-opencode-shell-env-"))
  tempDirs.push(root)
  await mkdir(path.join(root, ".opencode", "plugins"), { recursive: true })
  return root
}
