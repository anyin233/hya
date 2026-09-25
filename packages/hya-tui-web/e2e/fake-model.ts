// Scriptable OpenAI-compatible Chat Completions fake for browser TUI specs.
//
// hya's `openai-compatible` provider route only decodes what
// `crates/hya-provider/src/openai/decoder.rs` (`OpenAiChatDecoder`) reads
// from each SSE chunk: `choices[0].delta.content`, `choices[0].delta.tool_calls`
// (`index`/`id`/`function.name`/`function.arguments`), `choices[0].finish_reason`,
// and a trailing `usage` object (`completion_tokens_details.reasoning_tokens`
// for the thinking split). It does not read `reasoning_content` or any other
// reasoning field, so this fake has no reasoning/thinking step type — a
// scripted "reasoning" chunk would silently be dropped by hya, not rendered.
//
// This mirrors the Rust reference implementation
// (`crates/hya-e2e/src/fake_llm.rs`): an ordered queue of `Step`s consumed one
// per model request, optional routing by a marker substring found in the
// request's `system`-role content (so a spec can script independent flows for
// concurrent agents), and recorded request bodies for assertions. It runs in
// the Playwright (Node/Bun) process itself instead of as a separate Rust
// binary, so specs can start one per test with no extra process to build.

import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http"

/** Stream assistant text, then finish with `stop`. */
export type TextStep = {
  type: "text"
  text: string
  /** Split `text` into chunks of this many UTF-16 units per SSE delta (default: one delta for the whole string). */
  chunkSize?: number
  /** Delay in ms before each chunk (including the first), so streaming is observable. Default: 0. */
  delayMs?: number
}

/** One tool call the fake model "makes". */
export type ToolCall = {
  /** Canonical tool name (for example `read` or `glob`). */
  name: string
  /** JSON arguments object for the call. */
  arguments: unknown
}

/** Stream one or more tool calls, then finish with `tool_calls`. */
export type ToolCallsStep = {
  type: "toolCalls"
  calls: ToolCall[]
}

/** Fail the request before a stream opens, with a non-2xx HTTP status. */
export type HttpErrorStep = {
  type: "httpError"
  status: number
}

/**
 * Hold the connection open with no bytes written, so a spec can observe a
 * busy/working state (and test cancellation) before the turn ever finishes.
 * Resolved by calling `release()` on the fake (releases the oldest pending
 * hang), or automatically after `ms` (default: never) with an empty `stop`
 * reply, so a spec that forgets to release one does not hang forever.
 */
export type HangStep = {
  type: "hang"
  ms?: number
}

export type Step = TextStep | ToolCallsStep | HttpErrorStep | HangStep

/** Build a text step. */
export function textStep(text: string, options?: { chunkSize?: number; delayMs?: number }): TextStep {
  return { type: "text", text, ...options }
}

/** Build a single-tool-call step. */
export function toolStep(name: string, args: unknown): ToolCallsStep {
  return { type: "toolCalls", calls: [{ name, arguments: args }] }
}

/** Build a multi-tool-call step. */
export function toolsStep(calls: ToolCall[]): ToolCallsStep {
  return { type: "toolCalls", calls }
}

/** Build an HTTP error step. */
export function httpErrorStep(status: number): HttpErrorStep {
  return { type: "httpError", status }
}

/** Build a hang step. */
export function hangStep(ms?: number): HangStep {
  return { type: "hang", ms }
}

type Usage = { prompt: number; completion: number; reasoning: number }

type Route = {
  marker: string
  steps: Step[]
  requests: unknown[]
}

export type FakeModel = {
  /** `http://127.0.0.1:<port>/v1` — OpenAI-compatible base URL for a provider's `base_url`. */
  baseUrl: string
  /** Recorded request bodies (unrouted or matched by no route), in arrival order. */
  requests(): unknown[]
  /** Recorded request bodies attributed to `marker`'s route, or `undefined` if never registered. */
  routeRequests(marker: string): unknown[] | undefined
  /** Append more steps to the shared (unrouted) queue. */
  push(steps: Step[]): void
  /**
   * Pin `steps` to requests whose concatenated `system`-role content contains
   * `marker`. An exhausted route does not fall back to the shared queue.
   */
  route(marker: string, steps: Step[]): void
  /** Attach `usage` to every streamed response's finishing chunk from now on. */
  setUsage(usage: Usage): void
  /** Release the oldest pending `hang` step across all in-flight requests. */
  release(): void
  /** Number of pending (unreleased) hangs currently holding a connection open. */
  pendingHangs(): number
  /** Stop the HTTP server. */
  stop(): Promise<void>
}

function systemText(body: unknown): string {
  if (typeof body !== "object" || body === null) return ""
  const messages = (body as Record<string, unknown>).messages
  if (!Array.isArray(messages)) return ""
  return messages
    .filter((m) => m && typeof m === "object" && (m as Record<string, unknown>).role === "system")
    .map((m) => (m as Record<string, unknown>).content)
    .filter((c): c is string => typeof c === "string")
    .join("\n")
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/** Start the fake model server. `initial` seeds the shared (unrouted) queue. */
export async function startFakeModel(initial: Step[] = []): Promise<FakeModel> {
  const scripts: Step[] = [...initial]
  const requests: unknown[] = []
  const routes: Route[] = []
  let usage: Usage | undefined
  const hangs: Array<() => void> = []

  function popStep(body: unknown): { step: Step | undefined; route: Route | undefined } {
    const system = systemText(body)
    const route = routes.find((r) => system.includes(r.marker))
    if (route) {
      route.requests.push(body)
      return { step: route.steps.shift(), route }
    }
    return { step: scripts.shift(), route: undefined }
  }

  function writeSse(res: ServerResponse, frame: unknown): void {
    res.write(`data: ${JSON.stringify(frame)}\n\n`)
  }

  function withUsage(frame: Record<string, unknown>): Record<string, unknown> {
    if (!usage) return frame
    return {
      ...frame,
      usage: {
        prompt_tokens: usage.prompt,
        completion_tokens: usage.completion,
        total_tokens: usage.prompt + usage.completion,
        completion_tokens_details: { reasoning_tokens: usage.reasoning },
      },
    }
  }

  async function streamText(res: ServerResponse, step: TextStep): Promise<void> {
    writeSse(res, { choices: [{ delta: { role: "assistant", content: "" }, finish_reason: null }] })
    const chunkSize = Math.max(step.chunkSize ?? step.text.length, 1)
    const delay = step.delayMs ?? 0
    const chunks = step.text.length > 0 ? Math.ceil(step.text.length / chunkSize) : 1
    for (let chunk = 0; chunk < chunks; chunk++) {
      if (delay > 0) await sleep(delay)
      const text = step.text.slice(chunk * chunkSize, (chunk + 1) * chunkSize)
      writeSse(res, { choices: [{ delta: { content: text }, finish_reason: null }] })
    }
    writeSse(res, withUsage({ choices: [{ delta: {}, finish_reason: "stop" }] }))
    res.write("data: [DONE]\n\n")
  }

  function streamToolCalls(res: ServerResponse, step: ToolCallsStep): void {
    step.calls.forEach((call, index) => {
      writeSse(res, {
        choices: [
          {
            delta: {
              tool_calls: [
                { index, id: `call_${index}`, type: "function", function: { name: call.name, arguments: "" } },
              ],
            },
            finish_reason: null,
          },
        ],
      })
      writeSse(res, {
        choices: [
          {
            delta: { tool_calls: [{ index, function: { arguments: JSON.stringify(call.arguments) } }] },
            finish_reason: null,
          },
        ],
      })
    })
    writeSse(res, withUsage({ choices: [{ delta: {}, finish_reason: "tool_calls" }] }))
    res.write("data: [DONE]\n\n")
  }

  async function handleHang(res: ServerResponse, step: HangStep): Promise<void> {
    await new Promise<void>((resolve) => {
      hangs.push(resolve)
      if (step.ms !== undefined) setTimeout(resolve, step.ms)
    })
    writeSse(res, { choices: [{ delta: { role: "assistant", content: "" }, finish_reason: null }] })
    writeSse(res, withUsage({ choices: [{ delta: {}, finish_reason: "stop" }] }))
    res.write("data: [DONE]\n\n")
  }

  async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    if (req.method !== "POST" || req.url !== "/v1/chat/completions") {
      res.writeHead(404).end()
      return
    }
    const chunks: Buffer[] = []
    for await (const chunk of req) chunks.push(chunk as Buffer)
    let body: unknown = {}
    try {
      body = JSON.parse(Buffer.concat(chunks).toString("utf8"))
    } catch {
      body = {}
    }
    requests.push(body)
    const { step } = popStep(body)
    if (!step) {
      // Scripts exhausted: terminate the turn cleanly instead of hanging.
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" })
      writeSse(res, { choices: [{ delta: { content: "" }, finish_reason: null }] })
      writeSse(res, withUsage({ choices: [{ delta: {}, finish_reason: "stop" }] }))
      res.write("data: [DONE]\n\n")
      res.end()
      return
    }
    if (step.type === "httpError") {
      res.writeHead(step.status, { "content-type": "text/plain" }).end("scripted pre-stream failure")
      return
    }
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" })
    if (step.type === "text") await streamText(res, step)
    else if (step.type === "toolCalls") streamToolCalls(res, step)
    else await handleHang(res, step)
    res.end()
  }

  const server: Server = createServer((req, res) => {
    handle(req, res).catch(() => {
      if (!res.headersSent) res.writeHead(500)
      res.end()
    })
  })
  const port = await new Promise<number>((resolve, reject) => {
    server.once("error", reject)
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address()
      if (addr && typeof addr === "object") resolve(addr.port)
      else reject(new Error("fake model server has no port"))
    })
  })

  return {
    baseUrl: `http://127.0.0.1:${port}/v1`,
    requests: () => [...requests],
    routeRequests: (marker) => routes.find((r) => r.marker === marker)?.requests.map((r) => r),
    push: (steps) => scripts.push(...steps),
    route: (marker, steps) => routes.push({ marker, steps: [...steps], requests: [] }),
    setUsage: (next) => {
      usage = next
    },
    release: () => {
      hangs.shift()?.()
    },
    pendingHangs: () => hangs.length,
    stop: () =>
      new Promise((resolve, reject) => {
        server.close((err) => (err ? reject(err) : resolve()))
      }),
  }
}
