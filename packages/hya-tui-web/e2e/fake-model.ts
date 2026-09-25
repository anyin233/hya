// Scriptable OpenAI model fake for browser TUI specs. It speaks two wire
// protocols, chosen per request by its path:
//
// - `POST /v1/chat/completions` — OpenAI Chat Completions, for hya's
//   `openai-compatible` provider kind. hya only decodes what
//   `crates/hya-provider/src/openai/decoder.rs` (`OpenAiChatDecoder`) reads
//   from each SSE chunk: `choices[0].delta.content`, `choices[0].delta.tool_calls`
//   (`index`/`id`/`function.name`/`function.arguments`), `choices[0].finish_reason`,
//   and a trailing `usage` object. It does not read `reasoning_content`, so a
//   `reasoningStep` on this protocol streams only its answer text.
// - `POST /v1/responses` — the OpenAI Responses API, for hya's
//   `openai-response` provider kind. `OpenAiResponsesDecoder`
//   (`crates/hya-provider/src/openai/response_decoder.rs`) reads
//   `response.reasoning_summary_text.delta`, `response.output_item.added/done`
//   (reasoning and function_call items), `response.function_call_arguments.delta`,
//   `response.output_text.delta/done`, and the typed terminal
//   `response.completed` / `response.incomplete` (length). Reasoning is
//   therefore only rendered on this protocol. The stream must end with a typed
//   terminal event; `[DONE]` is never sent.
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
  /** Finish reason. `length` ends the reply as if it hit the output limit. Default: `stop`. */
  finish?: "stop" | "length"
}

/**
 * Stream reasoning (thinking) text, then the answer text, then finish `stop`.
 * Reasoning is only decoded by hya on the Responses protocol; the chat
 * protocol streams just `text`.
 */
export type ReasoningStep = {
  type: "reasoning"
  reasoning: string
  text: string
  /** Chunk size for both the reasoning and the answer (default: whole strings). */
  chunkSize?: number
  /** Delay in ms before each chunk. Default: 0. */
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

export type Step = TextStep | ReasoningStep | ToolCallsStep | HttpErrorStep | HangStep

/** Wire protocol of one request, chosen by its path. */
export type Protocol = "chat" | "responses"

/** Build a text step. */
export function textStep(text: string, options?: { chunkSize?: number; delayMs?: number; finish?: "stop" | "length" }): TextStep {
  return { type: "text", text, ...options }
}

/** Build a reasoning step: thinking text first, then the answer. */
export function reasoningStep(reasoning: string, text: string, options?: { chunkSize?: number; delayMs?: number }): ReasoningStep {
  return { type: "reasoning", reasoning, text, ...options }
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
  /** Attach `usage` to every streamed response's finishing chunk from now on (title replies excepted). */
  setUsage(usage: Usage): void
  /**
   * Reply to the backend's background session-title requests (the fixed
   * `title` agent, recognized by its system prompt) with `title`. Title
   * requests never consume the shared queue or a route, never appear in
   * `requests()`, and carry no usage, so scripted specs stay deterministic
   * whenever the title task runs; by default they get an empty reply and the
   * session stays untitled.
   */
  setTitleReply(title: string): void
  /** Background title request bodies, in arrival order. */
  titleRequests(): unknown[]
  /** Release the oldest pending `hang` step across all in-flight requests. */
  release(): void
  /** Number of pending (unreleased) hangs currently holding a connection open. */
  pendingHangs(): number
  /** Stop the HTTP server. */
  stop(): Promise<void>
}

/** System text of a request: chat `system` messages, or Responses `instructions` and `system` input items. */
function systemText(body: unknown): string {
  if (typeof body !== "object" || body === null) return ""
  const record = body as Record<string, unknown>
  const instructions = typeof record.instructions === "string" ? [record.instructions] : []
  const messages = Array.isArray(record.messages) ? record.messages : Array.isArray(record.input) ? record.input : []
  return [
    ...instructions,
    ...messages
      .filter((m) => m && typeof m === "object" && (m as Record<string, unknown>).role === "system")
      .map((m) => (m as Record<string, unknown>).content)
      .filter((c): c is string => typeof c === "string"),
  ].join("\n")
}

function chunked(text: string, size: number | undefined): string[] {
  const step = Math.max(size ?? text.length, 1)
  if (text.length === 0) return [""]
  const chunks: string[] = []
  for (let at = 0; at < text.length; at += step) chunks.push(text.slice(at, at + step))
  return chunks
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/** Opening of the fixed `title` agent's system prompt (the `core-agents` preset). */
export const titleAgentMarker = "You are a title generator."

/** Start the fake model server. `initial` seeds the shared (unrouted) queue. */
export async function startFakeModel(initial: Step[] = []): Promise<FakeModel> {
  const scripts: Step[] = [...initial]
  const requests: unknown[] = []
  const routes: Route[] = []
  let usage: Usage | undefined
  const hangs: Array<() => void> = []
  const titleRequests: unknown[] = []
  let titleReply = ""
  /** Responses that carry no usage (title replies). */
  const noUsage = new WeakSet<ServerResponse>()

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

  function withUsage(res: ServerResponse, frame: Record<string, unknown>): Record<string, unknown> {
    if (!usage || noUsage.has(res)) return frame
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

  // ---- Chat Completions (`/v1/chat/completions`) -------------------------

  function chatFinish(res: ServerResponse, finish: string): void {
    writeSse(res, withUsage(res, { choices: [{ delta: {}, finish_reason: finish }] }))
    res.write("data: [DONE]\n\n")
  }

  async function chatText(res: ServerResponse, text: string, options: { chunkSize?: number; delayMs?: number; finish?: string }): Promise<void> {
    writeSse(res, { choices: [{ delta: { role: "assistant", content: "" }, finish_reason: null }] })
    for (const chunk of chunked(text, options.chunkSize)) {
      if (options.delayMs) await sleep(options.delayMs)
      writeSse(res, { choices: [{ delta: { content: chunk }, finish_reason: null }] })
    }
    chatFinish(res, options.finish ?? "stop")
  }

  function chatToolCalls(res: ServerResponse, step: ToolCallsStep): void {
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
    chatFinish(res, "tool_calls")
  }

  // ---- Responses API (`/v1/responses`) -----------------------------------

  function responsesUsage(res: ServerResponse): Record<string, unknown> | undefined {
    if (!usage || noUsage.has(res)) return undefined
    return {
      input_tokens: usage.prompt,
      output_tokens: usage.completion,
      total_tokens: usage.prompt + usage.completion,
      output_tokens_details: { reasoning_tokens: usage.reasoning },
    }
  }

  function responsesFinish(res: ServerResponse, finish: "stop" | "length"): void {
    const response = { id: "resp_fake", status: finish === "length" ? "incomplete" : "completed", usage: responsesUsage(res) }
    writeSse(res, { type: finish === "length" ? "response.incomplete" : "response.completed", response })
  }

  async function responsesText(res: ServerResponse, index: number, text: string, options: { chunkSize?: number; delayMs?: number }): Promise<void> {
    for (const chunk of chunked(text, options.chunkSize)) {
      if (options.delayMs) await sleep(options.delayMs)
      writeSse(res, { type: "response.output_text.delta", output_index: index, content_index: 0, delta: chunk })
    }
    writeSse(res, { type: "response.output_text.done", output_index: index, content_index: 0, text })
  }

  async function responsesReasoning(res: ServerResponse, step: ReasoningStep): Promise<void> {
    writeSse(res, { type: "response.output_item.added", output_index: 0, item: { type: "reasoning", id: "rs_fake", summary: [] } })
    for (const chunk of chunked(step.reasoning, step.chunkSize)) {
      if (step.delayMs) await sleep(step.delayMs)
      writeSse(res, { type: "response.reasoning_summary_text.delta", output_index: 0, summary_index: 0, delta: chunk })
    }
    writeSse(res, {
      type: "response.output_item.done",
      output_index: 0,
      item: { type: "reasoning", id: "rs_fake", summary: [{ type: "summary_text", text: step.reasoning }] },
    })
    await responsesText(res, 1, step.text, step)
    responsesFinish(res, "stop")
  }

  function responsesToolCalls(res: ServerResponse, step: ToolCallsStep): void {
    step.calls.forEach((call, index) => {
      const item = { type: "function_call", id: `fc_${index}`, call_id: `call_${index}`, name: call.name, arguments: "" }
      const args = JSON.stringify(call.arguments)
      writeSse(res, { type: "response.output_item.added", output_index: index, item })
      writeSse(res, { type: "response.function_call_arguments.delta", output_index: index, delta: args })
      writeSse(res, { type: "response.output_item.done", output_index: index, item: { ...item, arguments: args } })
    })
    responsesFinish(res, "stop")
  }

  // ---- Steps ---------------------------------------------------------------

  async function streamStep(res: ServerResponse, protocol: Protocol, step: Exclude<Step, HttpErrorStep | HangStep>): Promise<void> {
    if (protocol === "chat") {
      if (step.type === "text") await chatText(res, step.text, step)
      else if (step.type === "reasoning") await chatText(res, step.text, step)
      else chatToolCalls(res, step)
      return
    }
    if (step.type === "text") {
      await responsesText(res, 0, step.text, step)
      responsesFinish(res, step.finish ?? "stop")
    } else if (step.type === "reasoning") await responsesReasoning(res, step)
    else responsesToolCalls(res, step)
  }

  async function handleHang(res: ServerResponse, protocol: Protocol, step: HangStep): Promise<void> {
    await new Promise<void>((resolve) => {
      hangs.push(resolve)
      if (step.ms !== undefined) setTimeout(resolve, step.ms)
    })
    await streamStep(res, protocol, { type: "text", text: "" })
  }

  const paths: Record<string, Protocol> = { "/v1/chat/completions": "chat", "/v1/responses": "responses" }

  async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const protocol = paths[req.url ?? ""]
    if (req.method !== "POST" || !protocol) {
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
    if (systemText(body).includes(titleAgentMarker)) {
      titleRequests.push(body)
      noUsage.add(res)
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" })
      await streamStep(res, protocol, { type: "text", text: titleReply })
      res.end()
      return
    }
    requests.push(body)
    const { step } = popStep(body)
    if (step?.type === "httpError") {
      res.writeHead(step.status, { "content-type": "text/plain" }).end("scripted pre-stream failure")
      return
    }
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" })
    // Scripts exhausted: terminate the turn cleanly (empty reply) instead of hanging.
    if (!step) await streamStep(res, protocol, { type: "text", text: "" })
    else if (step.type === "hang") await handleHang(res, protocol, step)
    else await streamStep(res, protocol, step)
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
    setTitleReply: (title) => {
      titleReply = title
    },
    titleRequests: () => [...titleRequests],
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
