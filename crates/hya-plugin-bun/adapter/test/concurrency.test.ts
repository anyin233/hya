import { afterEach, expect, test } from "bun:test"
import { writeFile } from "node:fs/promises"
import path from "node:path"
import { runAdapter } from "../src/runtime"
import { cleanupTempDirs, makeTempDir, initializeRequest, shutdownRequest } from "./helpers"
afterEach(cleanupTempDirs)

test("a second call completes while the first call is pending", async () => {
  const root = await makeTempDir()
  const extension = path.join(root, "pending.ts")
  let release!: () => void
  const pending = new Promise<void>((resolve) => { release = resolve })
  await writeFile(extension, `export default { id: "pending", server: async () => ({ tool: { held: { description: "held", execute: () => globalThis[${JSON.stringify(root)}] }, quick: { description: "quick", execute: () => "quick" } } }) }`)
  Object.assign(globalThis, { [root]: pending })
  let controller!: ReadableStreamDefaultController<Uint8Array>
  const input = new ReadableStream<Uint8Array>({ start(value) { controller = value } })
  const send = (request: unknown) => controller.enqueue(new TextEncoder().encode(JSON.stringify(request) + "\n"))
  const responses: number[] = []
  let second!: () => void
  const completed = new Promise<void>((resolve) => { second = resolve })
  const running = runAdapter({ input, version: "test", extensions: [extension], stderr: { write: () => {} }, stdout: { write: (line) => { const response = JSON.parse(line); responses.push(response.id); if (response.id === 3) second() } } })
  send(initializeRequest(1))
  send({ jsonrpc: "2.0", id: 2, method: "tool/call", params: { session: "s", call: "held", tool: "held", input: {} } })
  send({ jsonrpc: "2.0", id: 3, method: "tool/call", params: { session: "s", call: "quick", tool: "quick", input: {} } })
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    await Promise.race([completed, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("second call blocked behind pending first call")), 1000) })])
    expect(responses).toEqual([1, 3])
  } finally {
    clearTimeout(timer)
    release()
    send(shutdownRequest(4))
    controller.close()
    await running
    Reflect.deleteProperty(globalThis, root)
  }
})
