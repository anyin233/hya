/**
 * The shared extension host: `bun <sdk>/src/main.ts`, started once by the TUI
 * (packages/hya-tui/src/extensions/manager.ts), never by a bundle. It serves
 * the host channel (newline-delimited JSON-RPC 2.0 in both directions on
 * stdin/stdout; docs/tui-extensions.md "Wire contract"):
 *
 * - TUI → host: `host/load {ext, entry, permissions, jit?}` bundles the entry
 *   and creates its VM; `host/unload {ext}`; `ext/call {ext, request}` runs
 *   one extension request and answers `{response}`.
 * - host → TUI: `fs/read`, `fs/list`, `fs/stat`, `fs/watch`, `fs/unwatch`
 *   `{ext, …}`, issued by an extension's `api.fs`.
 *
 * VMs live in one worker thread (src/worker.ts); this thread bundles and
 * relays, so the channel stays responsive while a VM runs. A `jit` load (an
 * extension the user trusts) runs on JavaScriptCore instead, on its own
 * thread (src/jitThread.ts). Extension console output goes to stderr as
 * `{"ext","line"}` JSON lines.
 */
import { encodeFrame, MAX_FRAME_BYTES, parseFrame, type JsonValue, type RpcEnvelope, type RpcId } from "./protocol"
import { bundleExtension } from "./host"
import type { FromWorker, ToWorker } from "./hostMessages"
import { JitThread } from "./jitThread"

// One VM thread. More start slower, not faster: each extra thread made 20 VMs
// ready ~25 ms later on macOS and ~120 ms later on Linux (Docker), and a
// runaway VM is bounded anyway (2 s deadline; the TUI restarts the extension
// within its budget, then leaves it failed).
const worker = new Worker(new URL("./worker.ts", import.meta.url))
/** Extensions with a VM in the worker (or one being created). */
const loaded = new Set<string>()
/** Trusted extensions, each on its own JIT thread. */
const jits = new Map<string, JitThread>()
let nextSeq = 1
const waiting = new Map<number, PromiseWithResolvers<{ response?: string }>>()
/** Requests this host sent to the TUI, by id: the call each answer goes to, and the JIT thread that made it (else the VM worker). */
const asked = new Map<RpcId, { call: number; jit?: JitThread }>()
let nextId = 1
/** Loads still bundling or creating their VM, by extension. */
const loading = new Map<string, Promise<void>>()
/** Requests being served, answered before the host exits. */
const inflight = new Set<Promise<void>>()

const writeOut = process.stdout.write.bind(process.stdout)
function send(frame: RpcEnvelope): void {
  writeOut(encodeFrame(frame))
}

function log(ext: string, line: string): void {
  process.stderr.write(`${JSON.stringify({ ext, line })}\n`)
}

function askTui(ext: string, method: string, params: Record<string, unknown>, target: { call: number; jit?: JitThread }): void {
  const id = `h${nextId++}`
  asked.set(id, target)
  send({ jsonrpc: "2.0", id, method, params: { ext, ...params } as JsonValue })
}

worker.onmessage = (event: MessageEvent<FromWorker>) => {
  const message = event.data
  if (message.type === "log") {
    log(message.ext, message.line)
    return
  }
  if (message.type === "host") {
    askTui(message.ext, message.method, message.params, { call: message.call })
    return
  }
  const waiter = waiting.get(message.seq)
  waiting.delete(message.seq)
  if (message.error !== undefined) waiter?.reject(new Error(message.error))
  else waiter?.resolve(message.response === undefined ? {} : { response: message.response })
}

/** Post to the worker and wait for its `done`. */
function ask(message: (seq: number) => ToWorker): Promise<{ response?: string }> {
  const seq = nextSeq++
  const waiter = Promise.withResolvers<{ response?: string }>()
  waiting.set(seq, waiter)
  worker.postMessage(message(seq))
  return waiter.promise
}

type Params = Readonly<Record<string, JsonValue | undefined>>

function field(params: Params, name: string): string {
  const value = params[name]
  if (typeof value !== "string" || !value) throw new Error(`${name} required`)
  return value
}

async function serve(method: string, params: Params): Promise<JsonValue> {
  switch (method) {
    case "host/load": {
      const ext = field(params, "ext")
      await unload(ext)
      const jit = params.jit === true
      if (!jit) loaded.add(ext)
      // Calls that arrive while the entry is bundled wait for it (the worker sees the load first).
      const load = bundleExtension(field(params, "entry")).then(async (script) => {
        if (!jit) {
          await ask((seq) => ({ type: "load", seq, ext, script }))
          return
        }
        const thread: JitThread = new JitThread(ext, {
          host: (call, hostMethod, hostParams) => askTui(ext, hostMethod, hostParams, { call, jit: thread }),
          log: (line) => log(ext, line),
        })
        jits.set(ext, thread)
        await thread.load(script)
      })
      loading.set(ext, load)
      try {
        await load
      } catch (error) {
        if (loading.get(ext) === load) {
          loaded.delete(ext)
          jits.get(ext)?.stop()
          jits.delete(ext)
        }
        throw error
      } finally {
        if (loading.get(ext) === load) loading.delete(ext)
      }
      return null
    }
    case "host/unload": {
      const ext = field(params, "ext")
      await loading.get(ext)?.catch(() => undefined)
      await unload(ext)
      return null
    }
    case "ext/call": {
      const ext = field(params, "ext")
      await loading.get(ext)
      const jit = jits.get(ext)
      if (!jit && !loaded.has(ext)) throw new Error(`extension not loaded: ${ext}`)
      const text = JSON.stringify(params.request ?? null)
      const request = parseFrame(text)
      if (!request.ok || !("method" in request.frame)) throw new Error(`request required${request.ok ? "" : `: ${request.message}`}`)
      const id = request.frame.id
      const response = jit ? await jit.call(id, text) : (await ask((seq) => ({ type: "call", seq, ext, id, request: text }))).response
      // The VM's answer is untrusted: it must be one valid response frame.
      const checked = parseFrame(response ?? "")
      if (!checked.ok || "method" in checked.frame) throw new Error(`invalid extension response: ${checked.ok ? "not a response" : checked.message}`)
      return { response: checked.frame as JsonValue }
    }
    default:
      throw new Error(`method not found: ${method}`)
  }
}

async function unload(ext: string): Promise<void> {
  jits.get(ext)?.stop()
  jits.delete(ext)
  if (loaded.delete(ext)) await ask((seq) => ({ type: "unload", seq, ext }))
}

function receive(frame: RpcEnvelope): void {
  if (!("method" in frame)) {
    // An answer to one of this host's fs requests.
    if (frame.id === null) return
    const target = asked.get(frame.id)
    asked.delete(frame.id)
    if (!target) return
    const result = "error" in frame ? { error: frame.error.message } : { value: frame.result }
    if (target.jit) target.jit.hostResult(target.call, result)
    else worker.postMessage({ type: "hostResult", call: target.call, ...result } satisfies ToWorker)
    return
  }
  // Every host method takes named parameters.
  const params: Params = typeof frame.params === "object" && frame.params !== null && !Array.isArray(frame.params) ? frame.params as Params : {}
  const served = serve(frame.method, params).then(
    (result) => send({ jsonrpc: "2.0", id: frame.id, result }),
    (error: unknown) => send({ jsonrpc: "2.0", id: frame.id, error: { code: -32000, message: error instanceof Error ? error.message : String(error) } }),
  )
  inflight.add(served)
  void served.finally(() => inflight.delete(served))
}

const decoder = new TextDecoder()
let buffer = ""
for await (const chunk of Bun.stdin.stream()) {
  buffer += decoder.decode(chunk, { stream: true })
  let newline: number
  while ((newline = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, newline).replace(/\r$/, "")
    buffer = buffer.slice(newline + 1)
    if (!line) continue
    const parsed = parseFrame(line)
    if (!parsed.ok) {
      process.stderr.write(`${JSON.stringify({ line: `host: dropped a frame: ${parsed.message}` })}\n`)
      continue
    }
    receive(parsed.frame)
  }
  if (buffer.length > MAX_FRAME_BYTES) {
    process.stderr.write(`${JSON.stringify({ line: `host: frame exceeds ${MAX_FRAME_BYTES} bytes` })}\n`)
    process.exit(1)
  }
}
// The TUI closed the channel: answer what is still running, then end (the worker would keep the process alive).
await Promise.all(inflight)
process.exit(0)
