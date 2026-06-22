import { expect, test } from "bun:test"
import { z } from "zod"

const AdapterResponseSchema = z.object({
  jsonrpc: z.literal("2.0"),
  id: z.number().int(),
  result: z.unknown().optional(),
  error: z
    .object({
      code: z.number().int(),
      message: z.string(),
    })
    .optional(),
})

const InitializeResultSchema = z.object({
  protocol_version: z.literal(1),
  plugin: z.object({
    id: z.literal("opencode"),
    version: z.string(),
    kind: z.literal("opencode"),
  }),
  hooks: z.array(z.unknown()),
  tools: z.array(z.unknown()),
})

async function runAdapter(
  requests: readonly unknown[],
): Promise<readonly z.infer<typeof AdapterResponseSchema>[]> {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts"], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
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

test("initialize returns yaca opencode plugin identity", async () => {
  const responses = await runAdapter([
    {
      jsonrpc: "2.0",
      id: 11,
      method: "initialize",
      params: { protocol_version: 1, host: { name: "yaca", version: "0.0.0" } },
    },
    { jsonrpc: "2.0", id: 12, method: "shutdown", params: {} },
  ])

  expect(responses).toHaveLength(2)
  const first = responses[0]
  expect(first?.id).toBe(11)
  const result = InitializeResultSchema.parse(first?.result)
  expect(result.plugin.kind).toBe("opencode")
  expect(result.hooks).toEqual([])
  expect(result.tools).toEqual([])
})

test("unknown methods return JSON-RPC method-not-found errors", async () => {
  const responses = await runAdapter([
    { jsonrpc: "2.0", id: 21, method: "missing", params: {} },
    { jsonrpc: "2.0", id: 22, method: "shutdown", params: {} },
  ])

  expect(responses).toHaveLength(2)
  expect(responses[0]?.id).toBe(21)
  expect(responses[0]?.error?.code).toBe(-32601)
  expect(responses[1]?.id).toBe(22)
  expect(responses[1]?.result).toEqual({})
})
