/**
 * A trusted extension's thread in the shared host (src/main.ts): the bundled
 * script runs on JavaScriptCore with its JIT, in a fresh `node:vm` realm,
 * instead of in the QuickJS VM (src/worker.ts). One thread per extension, so
 * the main thread can stop a runaway handler by terminating it; there is no
 * memory cap (docs/tui-extensions.md, Security).
 *
 * The realm's global is created from a null-prototype object (a plain `{}`
 * would expose this thread's `Object`, and through it `Function`, to the
 * extension), and everything the bundle sees is defined inside the realm by
 * `prelude`. This side only calls the prelude's functions, captured before
 * the bundle runs, with strings and numbers, and keeps only string results:
 * no object of this thread ever becomes reachable from extension code.
 */
import { createContext, runInContext } from "node:vm"
import type { FromWorker, ToWorker } from "./hostMessages"
import { VM_DEADLINE_MS } from "./protocol"

declare const self: Worker

const post = (message: FromWorker): void => self.postMessage(message)

/** Realm side: host calls and console lines wait in queues this thread drains as JSON text. */
const prelude = `(() => {
  const calls = []
  const logs = []
  const waiting = new Map()
  const results = new Map()
  let nextCall = 1
  const text = (value) => { if (typeof value === "string") return value; try { return JSON.stringify(value) } catch { return String(value) } }
  const write = (...args) => { logs.push(args.map(text).join(" ").slice(0, 4000)) }
  globalThis.console = { log: write, info: write, warn: write, error: write, debug: write }
  globalThis.__hyaHost = (method, paramsJson) => new Promise((resolve, reject) => {
    const call = nextCall++
    waiting.set(call, { resolve, reject })
    calls.push([call, String(method), String(paramsJson)])
  })
  const failure = (id, error) => JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32000, message: error instanceof Error ? error.message : String(error) } })
  return [
    (key, id, json) => { Promise.resolve().then(() => globalThis.__hyaHandle(json)).then((out) => { results.set(key, typeof out === "string" ? out : failure(id, "handler returned no response")) }, (error) => { results.set(key, failure(id, error)) }) },
    (key) => { const out = results.get(key); results.delete(key); return out === undefined ? "" : out },
    () => { const out = JSON.stringify({ calls, logs }); calls.length = 0; logs.length = 0; return out },
    (call, ok, json) => { const waiter = waiting.get(call); waiting.delete(call); if (!waiter) return; if (ok) waiter.resolve(json); else waiter.reject(new Error(json)) },
  ]
})()`

type Start = (key: number, id: string | number, json: string) => void
type Poll = (key: number) => unknown
type Drain = () => unknown
type Settle = (call: number, ok: boolean, json: string) => void

let realm: { start: Start; poll: Poll; drain: Drain; settle: Settle } | undefined
let ext = ""
/** Host calls forwarded and not answered yet. */
let outstanding = 0
let nextKey = 1
/** The request being handled; the main thread sends the next only after this one's `done`. */
let current: { key: number; id: string | number; seq: number } | undefined

const failure = (id: string | number, message: string): string => JSON.stringify({ jsonrpc: "2.0", id, error: { code: -32000, message } })

function finish(response: string): void {
  if (!current) return
  post({ type: "done", seq: current.seq, response })
  current = undefined
}

/** Forward what the realm queued (host calls, console lines); answer the current request once it settled. */
function pump(): void {
  if (!realm) return
  const drained = realm.drain()
  if (typeof drained === "string") {
    const { calls, logs } = JSON.parse(drained) as { calls: [number, string, string][]; logs: string[] }
    for (const line of logs) post({ type: "log", ext, line })
    for (const [call, method, paramsJson] of calls) {
      outstanding += 1
      let params: Record<string, unknown> = {}
      try { params = JSON.parse(paramsJson) as Record<string, unknown> } catch { /* malformed: the TUI refuses it */ }
      post({ type: "host", call, ext, method, params })
    }
  }
  if (!current) return
  const out = realm.poll(current.key)
  if (typeof out === "string" && out) finish(out)
  else if (outstanding === 0) finish(failure(current.id, "handler never settles (extensions have no timers; only api.fs is asynchronous)"))
}

/** A macrotask later, so the realm's microtasks have all run: forward what they queued, see whether the request settled. */
const pumpSoon = (): void => { setImmediate(pump) }

/** A realm value's message, as text: reading it runs only realm code, which this thread's deadline bounds. */
function describe(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error) return String(error.message)
  return String(error)
}

self.onmessage = (event: MessageEvent<ToWorker>) => {
  const message = event.data
  switch (message.type) {
    case "load": {
      ext = message.ext
      const context = createContext(Object.create(null) as object)
      const [start, poll, drain, settle] = runInContext(prelude, context) as [Start, Poll, Drain, Settle]
      realm = { start, poll, drain, settle }
      try {
        runInContext(message.script, context, { timeout: VM_DEADLINE_MS, filename: "extension.js" })
      } catch (error) {
        realm = undefined
        post({ type: "done", seq: message.seq, error: `extension failed to load: ${describe(error)}` })
        return
      }
      pump()
      post({ type: "done", seq: message.seq })
      return
    }
    case "call":
      if (!realm) {
        post({ type: "done", seq: message.seq, error: `extension not loaded: ${message.ext}` })
        return
      }
      current = { key: nextKey++, id: message.id, seq: message.seq }
      realm.start(current.key, message.id, message.request)
      pumpSoon()
      return
    case "hostResult":
      outstanding -= 1
      realm?.settle(message.call, message.error === undefined, message.error ?? JSON.stringify(message.value ?? null))
      pumpSoon()
      return
    case "unload":
      // The main thread terminates this thread instead.
      return
  }
}
