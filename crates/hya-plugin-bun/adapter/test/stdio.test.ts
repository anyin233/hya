import { expect, test } from "bun:test"
import { runStdio } from "../src/stdio"
import { okResponse } from "../src/protocol"

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>((done) => { resolve = done })
  return { promise, resolve }
}

function fixture() {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  const input = new ReadableStream<Uint8Array>({ start(value) { controller = value } })
  const responses: { id: number; result?: unknown; error?: { code: number } }[] = []
  return {
    input, responses,
    stdout: { write: (line: string) => { responses.push(JSON.parse(line)) } },
    stderr: { write: () => {} },
    send: (id: number | undefined, method: string) => controller.enqueue(new TextEncoder().encode(JSON.stringify({ jsonrpc: "2.0", id, method, params: {} }) + "\n")),
    close: () => controller.close(),
  }
}

test("initialization is a barrier, shutdown drains calls and notifications before disposal", async () => {
  const f = fixture()
  const initialize = deferred()
  const held = deferred()
  const observe = deferred()
  const quick = deferred()
  const invoked: string[] = []
  const running = runStdio(f, async (request) => {
    invoked.push(request.method)
    if (request.method === "initialize") await initialize.promise
    if (request.method === "held") await held.promise
    if (request.method === "event") await observe.promise
    if (request.method === "quick") quick.resolve()
    return { response: request.id === undefined ? "" : okResponse(request.id, {}), shouldExit: request.method === "shutdown" }
  })
  f.send(1, "initialize")
  f.send(2, "held")
  f.send(undefined, "event")
  f.send(3, "quick")
  f.send(4, "shutdown")
  f.send(5, "after-shutdown")
  // Await a microtask, not a timing interval: initialize has synchronously entered.
  await Promise.resolve()
  expect(invoked).not.toContain("held")
  initialize.resolve()
  await quick.promise
  expect(invoked).not.toContain("shutdown")
  expect(invoked).toContain("event")
  held.resolve()
  observe.resolve()
  f.close()
  await running
  expect(invoked).toEqual(["initialize", "held", "event", "quick", "shutdown"])
  expect(f.responses.map((r) => r.id)).toEqual([1, 3, 2, 4])
})

test("a rejected request returns its own error and other requests still complete", async () => {
  const f = fixture()
  const running = runStdio(f, async (request) => {
    if (request.method === "fail") throw new Error("failed handler")
    return { response: okResponse(request.id!, {}), shouldExit: false }
  })
  f.send(1, "fail")
  f.send(2, "quick")
  f.close()
  await running
  expect(f.responses.find((r) => r.id === 1)?.error?.code).toBe(-32603)
  expect(f.responses.find((r) => r.id === 2)?.result).toEqual({})
})
