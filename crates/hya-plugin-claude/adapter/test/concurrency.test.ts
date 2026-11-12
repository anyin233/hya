import { afterEach, expect, test } from "bun:test"
import { writeFile } from "node:fs/promises"
import path from "node:path"
import { runAdapter } from "../src/runtime"
import { cleanupTempDirs, initializeRequest, makePluginDir } from "./helpers"
afterEach(cleanupTempDirs)

test("a second request completes while a Claude hook command is pending", async () => {
  const root = await makePluginDir({ hooksJson: { PreToolUse: [{ hooks: [{ type: "command", command: 'read release < "$CLAUDE_PLUGIN_ROOT/release"' }] }] } })
  const fifo = path.join(root, "release")
  await writeFile(path.join(root, "hooks/hooks.json"), JSON.stringify({ PreToolUse: [{ hooks: [{ type: "command", command: `read release < ${JSON.stringify(fifo)}` }] }] }))
  expect(Bun.spawnSync(["mkfifo", fifo]).exitCode).toBe(0)
  let controller!: ReadableStreamDefaultController<Uint8Array>
  const input = new ReadableStream<Uint8Array>({ start(value) { controller = value } })
  const send = (id: number, method: string, params: unknown) => controller.enqueue(new TextEncoder().encode(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n"))
  let second!: () => void
  const completed = new Promise<void>((resolve) => { second = resolve })
  const responses: number[] = []
  const running = runAdapter({ input, version: "test", pluginId: "test", pluginDir: root, stderr: { write: () => {} }, stdout: { write: (line) => { const response = JSON.parse(line); responses.push(response.id); if (response.id === 3) second() } } })
  controller.enqueue(new TextEncoder().encode(JSON.stringify(initializeRequest(1)) + "\n"))
  send(2, "hook/tool.execute.before", { session: "s", message: "m", call: "c", tool: "bash", input: {} })
  send(3, "tool/call", { session: "s", call: "c2", tool: "missing", input: {} })
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    await Promise.race([completed, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("second request blocked behind pending Claude hook")), 1000) })])
    expect(responses).toEqual([1, 3])
  } finally {
    clearTimeout(timer)
    await writeFile(fifo, "release\n")
    send(4, "shutdown", {})
    controller.close()
    await running
  }
})
