import { expect, test } from "bun:test"
import type { HyaClient, StreamFrame } from "../src/client"
import { SessionViews } from "../src/app/sessionViews"

const tick = async () => { for (let i = 0; i < 20; i++) await Promise.resolve() }
function harness() {
  const streams: { id: string; signal: AbortSignal; emit(frame: StreamFrame): void | Promise<void> }[] = []
  const client = {
    request: async <T>() => ({ id: "child", agent: "task", workdir: "/work", lastSeq: "1", members: [] }) as T,
    listMessages: async () => [{ id: "saved", role: "ROLE_USER", parts: [{ id: "saved-p", text: { text: "Saved prompt" } }] }],
    listEventsSince: async () => [],
    streamSession: async (id, _seq, emit, signal, onOpen) => {
      streams.push({ id, emit, signal })
      await onOpen?.()
      await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }))
    },
  } satisfies Pick<HyaClient, "request" | "listMessages" | "listEventsSince" | "streamSession">
  return { manager: new SessionViews(client), streams }
}
test("two viewers share replay and streaming, with cleanup only at the final release", async () => {
  const { manager, streams } = harness()
  const a = manager.acquire("server/parent", "child"), b = manager.acquire("server/parent", "child")
  await tick()
  expect(streams.length).toBe(1)
  expect(a.store).toBe(b.store)
  expect(a.store.state.messages[0]?.parts?.[0]?.text?.text).toBe("Saved prompt")
  const emit = streams[0]!.emit
  await emit({ event: { session: "child", seq: "2", messageStarted: { message: "live", role: "ROLE_ASSISTANT" } } })
  await emit({ event: { session: "child", partStarted: { message: "live", part: "p", kind: "text" } } })
  await emit({ event: { session: "child", partAppended: { message: "live", part: "p", textDelta: "Thinking live" } } })
  await Bun.sleep(25)
  expect(a.store.state.overlay.find((row) => row.id === "live")?.parts?.[0]?.text?.text).toBe("Thinking live")
  a.release(); a.release()
  expect(streams[0]!.signal.aborted).toBe(false)
  b.release()
  expect(streams[0]!.signal.aborted).toBe(true)
  manager.dispose()
})
test("parent/server scope isolates watches and ignores frames after a viewer closes", async () => {
  const { manager, streams } = harness()
  const a = manager.acquire("old/parent", "child"), b = manager.acquire("new/parent", "child")
  await tick()
  expect(streams.length).toBe(2)
  expect(a.store).not.toBe(b.store)
  a.release()
  await streams[0]!.emit({ event: { session: "child", seq: "99", messageStarted: { message: "stale", role: "ROLE_ASSISTANT" } } })
  expect(a.store.fold.lastSeq).toBe("1")
  expect(b.store.state.connected).toBe(true)
  manager.dispose()
  expect(streams.every((stream) => stream.signal.aborted)).toBe(true)
  b.release()
})

test("closing while metadata is in flight never starts a stream or applies the stale response", async () => {
  let resolve!: (value: unknown) => void
  const pending = new Promise<unknown>((done) => { resolve = done })
  let started = false
  const manager = new SessionViews({
    request: async <T>() => await pending as T,
    listMessages: async () => [], listEventsSince: async () => [],
    streamSession: async () => { started = true },
  })
  const view = manager.acquire("root", "child")
  expect(view.store.state.connected).toBe(false)
  view.release()
  resolve({ id: "child", agent: "task", workdir: "/work" })
  await tick()
  expect(started).toBe(false)
  expect(view.store.state.selected).toBeUndefined()
  manager.dispose()
})

test("resync reconnects from the last durable sequence and reloads the projection", async () => {
  const cursors: string[] = []
  let attempts = 0, reloads = 0
  let ready!: () => void
  const reopened = new Promise<void>((resolve) => { ready = resolve })
  const manager = new SessionViews({
    request: async <T>() => ({ id: "child", agent: "task", workdir: "/work", lastSeq: "1" }) as T,
    listMessages: async () => { reloads++; return [] },
    listEventsSince: async (_id, seq) => { cursors.push(seq); return [] },
    streamSession: async (_id, _seq, emit, signal, onOpen) => {
      attempts++
      await onOpen?.()
      if (attempts === 1) {
        await emit({ event: { session: "child", seq: "2", messageStarted: { message: "live", role: "ROLE_ASSISTANT" } } })
        await emit({ resync: { lastSeq: "3" } })
      } else {
        ready()
        await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }))
      }
    },
  })
  const view = manager.acquire("root", "child")
  try {
    await reopened
    expect(cursors).toEqual(["1", "2"])
    expect(reloads).toBeGreaterThanOrEqual(2)
    expect(view.store.state.connected).toBe(true)
    expect(view.store.state.status).toBe("")
  } finally { view.release(); manager.dispose() }
})
