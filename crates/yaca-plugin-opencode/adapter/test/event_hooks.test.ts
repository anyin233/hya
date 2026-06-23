import { afterEach, expect, test } from "bun:test"
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
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

test("event notification fans out to OpenCode event hooks", async () => {
  const root = await makeTempDir()
  const markerFile = path.join(root, "event.json")
  const pluginFile = path.join(root, "event.ts")
  await writeFile(
    pluginFile,
    [
      'import { writeFile } from "node:fs/promises"',
      "export default {",
      '  id: "event",',
      "  server: async () => ({",
      "    event: async (input) => {",
      "      await writeFile(process.env.YACA_EVENT_MARKER, JSON.stringify(input.event))",
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(
    root,
    pluginFile,
    [
      initializeRequest(1),
      {
        jsonrpc: "2.0",
        method: "event",
        params: {
          envelope: {
            seq: 9,
            ts_millis: 12,
            event: {
              type: "session_created",
              session: "session-1",
              parent: null,
              agent: "build",
              model: "openai/gpt-5",
              workdir: root,
            },
          },
        },
      },
      shutdownRequest(2),
    ],
    { YACA_EVENT_MARKER: markerFile },
  )

  expect(responses.map((response) => response.id)).toEqual([1, 2])
  const event = JSON.parse(await readFile(markerFile, "utf8")) as unknown
  expect(event).toEqual({
    id: "9",
    type: "session.created",
    properties: {
      info: {
        id: "session-1",
        projectID: "session-1",
        directory: root,
        title: "",
        version: "0",
        time: { created: 12, updated: 12 },
      },
    },
  })
})

test("command executed events map to OpenCode command events", async () => {
  const root = await makeTempDir()
  const markerFile = path.join(root, "command-event.json")
  const pluginFile = path.join(root, "command-event.ts")
  await writeFile(
    pluginFile,
    [
      'import { writeFile } from "node:fs/promises"',
      "export default {",
      '  id: "command-event",',
      "  server: async () => ({",
      "    event: async (input) => {",
      "      await writeFile(process.env.YACA_EVENT_MARKER, JSON.stringify(input.event))",
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  await runAdapter(
    root,
    pluginFile,
    [
      initializeRequest(11),
      {
        jsonrpc: "2.0",
        method: "event",
        params: {
          envelope: {
            seq: 19,
            ts_millis: 20,
            event: {
              type: "command_executed",
              session: "session-1",
              command: "review",
              arguments: "commit",
              message: "message-1",
            },
          },
        },
      },
      shutdownRequest(12),
    ],
    { YACA_EVENT_MARKER: markerFile },
  )

  const event = JSON.parse(await readFile(markerFile, "utf8")) as unknown
  expect(event).toEqual({
    id: "19",
    type: "command.executed",
    properties: {
      name: "review",
      sessionID: "session-1",
      arguments: "commit",
      messageID: "message-1",
    },
  })
})

async function runAdapter(
  root: string,
  pluginFile: string,
  requests: readonly unknown[],
  env?: Readonly<Record<string, string>>,
): Promise<readonly z.infer<typeof AdapterResponseSchema>[]> {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts"], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    env: {
      ...process.env,
      ...env,
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
  const root = await mkdtemp(path.join(tmpdir(), "yaca-opencode-event-"))
  tempDirs.push(root)
  await mkdir(path.join(root, ".opencode", "plugins"), { recursive: true })
  return root
}
