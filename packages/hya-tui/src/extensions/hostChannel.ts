/**
 * The TUI's end of the shared extension host (packages/hya-tui-sdk/src/main.ts):
 * one child process for every extension, newline-delimited JSON-RPC 2.0 in
 * both directions. It bounds frames and pending requests and times requests
 * out; the host's own requests (`fs/*`) go to `serve`. Extension console
 * output arrives on stderr as `{"ext","line"}` JSON and goes to `log`.
 * Restarts belong to the caller (extensions/manager.ts): one `HostChannel`
 * is one child lifetime.
 */

export interface ChildProcess {
  stdin: { write(chunk: string): unknown; end(): unknown }
  stdout: AsyncIterable<Uint8Array>
  stderr: AsyncIterable<Uint8Array>
  exited: Promise<number>
  kill(signal?: "SIGTERM" | "SIGKILL"): void
}

export type Spawn = (argv: string[]) => ChildProcess

export interface HostChannelOptions {
  argv: string[]
  spawn?: Spawn
  timeoutMs?: number
  maxLineBytes?: number
  maxPending?: number
  /** Answers a request from the host (`fs/read`, …); a rejection becomes its error response. */
  serve: (method: string, params: Readonly<Record<string, unknown>>) => Promise<unknown>
  /** One line of an extension's console output (`ext` undefined: the host's own). */
  log: (ext: string | undefined, line: string) => void
  /** Called once when the child ends for any reason other than `stop()`. */
  onExit: (reason: string) => void
}

export class HostChannelError extends Error {
  override name = "HostChannelError"
}

/** A request outlived its timeout; the host still runs. */
export class HostTimeoutError extends HostChannelError {
  override name = "HostTimeoutError"
}

/** No inherited environment: no tokens, keys, or paths reach extension code. */
// Bun's Subprocess types its streams more narrowly than the channel needs; the shapes it reads are the same.
const defaultSpawn: Spawn = (argv) => Bun.spawn(argv, { env: {}, stdin: "pipe", stdout: "pipe", stderr: "pipe" }) as unknown as ChildProcess

interface Pending { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: Timer; method: string }

export class HostChannel {
  private readonly child: ChildProcess
  private readonly pending = new Map<number, Pending>()
  private nextId = 1
  private ended: string | undefined
  private stopping = false
  private readonly timeoutMs: number
  private readonly maxLineBytes: number
  private readonly maxPending: number

  constructor(private readonly options: HostChannelOptions) {
    this.timeoutMs = options.timeoutMs ?? 5_000
    this.maxLineBytes = options.maxLineBytes ?? 2 * 1024 * 1024
    this.maxPending = options.maxPending ?? 512
    this.child = (options.spawn ?? defaultSpawn)(options.argv)
    void this.readStdout()
    void this.readStderr()
    void this.child.exited.then((code) => this.end(`exited with status ${code}`), () => this.end("exited"))
  }

  /** Why the host ended, or `undefined` while it runs. */
  get failure(): string | undefined { return this.ended }

  /** One request; a timeout rejects it with `HostTimeoutError` and leaves the host running. */
  request<T = unknown>(method: string, params: unknown, timeoutMs = this.timeoutMs): Promise<T> {
    if (this.ended) return Promise.reject(new HostChannelError(`extension host not running: ${this.ended}`))
    if (this.pending.size >= this.maxPending) return Promise.reject(new HostChannelError("too many pending extension requests"))
    const id = this.nextId++
    const { promise, resolve, reject } = Promise.withResolvers<T>()
    const timer = setTimeout(() => {
      this.pending.delete(id)
      reject(new HostTimeoutError(`${method} timed out after ${timeoutMs} ms`))
    }, timeoutMs)
    this.pending.set(id, { resolve: (value) => resolve(value as T), reject, timer, method })
    this.write({ jsonrpc: "2.0", id, method, params })
    return promise
  }

  /** Close the channel and end the host. Idempotent. */
  async stop(): Promise<void> {
    if (this.stopping) return
    this.stopping = true
    this.end("stopped")
    try { this.child.stdin.end() } catch { /* already closed */ }
    try { this.child.kill("SIGTERM") } catch { /* already exited */ }
    await this.child.exited.catch(() => undefined)
  }

  private write(frame: object): void {
    try {
      this.child.stdin.write(`${JSON.stringify(frame)}\n`)
    } catch (error) {
      this.fail(`write failed: ${error instanceof Error ? error.message : String(error)}`)
    }
  }

  private fail(reason: string): void {
    this.end(reason)
    try { this.child.kill("SIGKILL") } catch { /* already exited */ }
  }

  private end(reason: string): void {
    if (this.ended) return
    this.ended = reason
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer)
      pending.reject(new HostChannelError(`extension host ${reason} (during ${pending.method})`))
    }
    this.pending.clear()
    if (!this.stopping) this.options.onExit(reason)
  }

  private async readStdout(): Promise<void> {
    const decoder = new TextDecoder()
    let buffer = ""
    try {
      for await (const chunk of this.child.stdout) {
        buffer += decoder.decode(chunk, { stream: true })
        let newline: number
        while ((newline = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, newline).replace(/\r$/, "")
          buffer = buffer.slice(newline + 1)
          if (line) this.handle(line)
          if (this.ended) return
        }
        if (buffer.length > this.maxLineBytes) return this.fail(`frame exceeds ${this.maxLineBytes} bytes`)
      }
    } catch { /* the exit handler reports it */ }
  }

  private async readStderr(): Promise<void> {
    const decoder = new TextDecoder()
    let partial = ""
    const emit = (raw: string): void => {
      const line = raw.slice(0, 4_000)
      if (!line.trim()) return
      try {
        const parsed: unknown = JSON.parse(line)
        if (parsed && typeof parsed === "object" && "line" in parsed && typeof parsed.line === "string") {
          this.options.log("ext" in parsed && typeof parsed.ext === "string" ? parsed.ext : undefined, parsed.line)
          return
        }
      } catch { /* not a structured line: the host's own output */ }
      this.options.log(undefined, line)
    }
    try {
      for await (const chunk of this.child.stderr) {
        const lines = (partial + decoder.decode(chunk, { stream: true })).split("\n")
        partial = (lines.pop() ?? "").slice(-4_000)
        for (const line of lines) emit(line)
      }
      emit(partial)
    } catch { /* diagnostics only */ }
  }

  private handle(line: string): void {
    if (line.length > this.maxLineBytes) return this.fail(`frame exceeds ${this.maxLineBytes} bytes`)
    let message: unknown
    try { message = JSON.parse(line) } catch { return this.fail("sent invalid JSON") }
    if (!message || typeof message !== "object" || !("jsonrpc" in message) || message.jsonrpc !== "2.0" || !("id" in message)) return this.fail("sent a malformed JSON-RPC frame")
    const id = message.id
    if ("method" in message) {
      // The host asks (an extension's api.fs): answer, success or failure.
      const method = typeof message.method === "string" ? message.method : ""
      const params = "params" in message && message.params && typeof message.params === "object" && !Array.isArray(message.params) ? message.params as Readonly<Record<string, unknown>> : {}
      void this.options.serve(method, params).then(
        (result) => this.write({ jsonrpc: "2.0", id, result: result ?? null }),
        (error: unknown) => this.write({ jsonrpc: "2.0", id, error: { code: -32000, message: error instanceof Error ? error.message : String(error) } }),
      )
      return
    }
    const hasResult = "result" in message
    const error = "error" in message && message.error && typeof message.error === "object" && "message" in message.error && typeof message.error.message === "string" ? message.error.message : undefined
    if (hasResult === (error !== undefined)) return this.fail("sent a malformed JSON-RPC frame")
    if (typeof id !== "number") return this.fail("answered an unknown request id")
    const pending = this.pending.get(id)
    // A request that timed out may still be answered late: ignore that answer.
    if (!pending) return
    this.pending.delete(id)
    clearTimeout(pending.timer)
    if (error !== undefined) pending.reject(new HostChannelError(error))
    else pending.resolve("result" in message ? message.result : undefined)
  }
}
