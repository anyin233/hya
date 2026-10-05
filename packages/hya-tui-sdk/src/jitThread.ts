/**
 * The shared host's handle on one trusted extension's JIT thread
 * (src/jitWorker.ts). JavaScriptCore cannot interrupt a realm's async code, so
 * the deadline lives here: each request may execute for `VM_DEADLINE_MS`,
 * with the clock paused while it waits for `api.fs`; past it, the thread is
 * terminated and the extension answers `VM_STOPPED_ERROR_CODE` until it is
 * loaded again. Requests run one at a time.
 */
import type { FromWorker, ToWorker } from "./hostMessages"
import { VM_DEADLINE_MS, VM_STOPPED_ERROR_CODE, type RpcId } from "./protocol"

export interface JitEvents {
  /** An `api.fs` call of the extension, to forward to the TUI; answer it with `hostResult`. */
  host(call: number, method: string, params: Record<string, unknown>): void
  log(line: string): void
}

export class JitThread {
  private readonly worker = new Worker(new URL("./jitWorker.ts", import.meta.url))
  private nextSeq = 1
  private readonly waiting = new Map<number, PromiseWithResolvers<string | undefined>>()
  /** The tail of the request chain. */
  private chain: Promise<unknown> = Promise.resolve()
  /** Why the thread stopped serving, once it did. */
  private stopped: string | undefined
  /** The request or load being executed, with its remaining execution budget. */
  private active: { seq: number; remaining: number; since: number; timer: Timer | undefined } | undefined
  /** Host calls the active request is waiting for: its clock is paused while there are any. */
  private readonly hostCalls = new Set<number>()

  constructor(private readonly ext: string, private readonly events: JitEvents) {
    this.worker.onmessage = (event: MessageEvent<FromWorker>) => this.receive(event.data)
    this.worker.onerror = (event) => this.stop(`extension thread crashed: ${event.message}`)
  }

  /** Create the realm and run the bundle (its top level has the same deadline). */
  async load(script: string): Promise<void> {
    await this.post((seq) => ({ type: "load", seq, ext: this.ext, script }))
  }

  /** Run one extension request (`text`: the request frame) and resolve to its response frame. */
  call(id: RpcId, text: string): Promise<string> {
    const run = async (): Promise<string> => {
      if (this.stopped) return this.stoppedResponse(id)
      const response = await this.post((seq) => ({ type: "call", seq, ext: this.ext, id, request: text })).catch(() => undefined)
      return response ?? this.stoppedResponse(id)
    }
    const result = this.chain.then(run, run)
    this.chain = result
    return result
  }

  hostResult(call: number, result: { value: unknown } | { error: string }): void {
    if (this.stopped) return
    if (this.hostCalls.delete(call) && this.hostCalls.size === 0) this.resume()
    this.worker.postMessage({ type: "hostResult", call, ...result } satisfies ToWorker)
  }

  /** Terminate the thread; pending and later requests answer `VM_STOPPED_ERROR_CODE` with `reason`. */
  stop(reason = "extension unloaded"): void {
    if (this.stopped) return
    this.stopped = reason
    clearTimeout(this.active?.timer)
    this.active = undefined
    this.worker.terminate()
    for (const waiter of this.waiting.values()) waiter.reject(new Error(reason))
    this.waiting.clear()
  }

  private post(message: (seq: number) => ToWorker): Promise<string | undefined> {
    const seq = this.nextSeq++
    const waiter = Promise.withResolvers<string | undefined>()
    this.waiting.set(seq, waiter)
    this.active = { seq, remaining: VM_DEADLINE_MS, since: 0, timer: undefined }
    this.hostCalls.clear()
    this.resume()
    this.worker.postMessage(message(seq))
    return waiter.promise
  }

  private receive(message: FromWorker): void {
    if (this.stopped) return
    if (message.type === "log") {
      this.events.log(message.line)
      return
    }
    if (message.type === "host") {
      if (this.active && this.hostCalls.size === 0) this.pause()
      if (this.active) this.hostCalls.add(message.call)
      this.events.host(message.call, message.method, message.params)
      return
    }
    if (this.active?.seq === message.seq) {
      clearTimeout(this.active.timer)
      this.active = undefined
    }
    const waiter = this.waiting.get(message.seq)
    this.waiting.delete(message.seq)
    if (message.error !== undefined) waiter?.reject(new Error(message.error))
    else waiter?.resolve(message.response)
  }

  private resume(): void {
    const active = this.active
    if (!active) return
    active.since = performance.now()
    active.timer = setTimeout(() => this.stop(`extension exceeded its ${VM_DEADLINE_MS} ms limit`), active.remaining)
  }

  private pause(): void {
    const active = this.active
    if (!active) return
    clearTimeout(active.timer)
    active.timer = undefined
    active.remaining -= performance.now() - active.since
  }

  private stoppedResponse(id: RpcId): string {
    return JSON.stringify({ jsonrpc: "2.0", id, error: { code: VM_STOPPED_ERROR_CODE, message: this.stopped ?? "extension stopped" } })
  }
}
