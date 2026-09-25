/** Small HTTP/JSON client for the shared hya.v1 server contract. */
export interface SessionInfo {
  id: string
  agent: string
  workdir: string
  title?: string
  model?: { providerId?: string; modelId?: string; variant?: string }
  busy?: boolean
  lastSeq?: string
}

export interface TurnInfo {
  id: string
  state: string
  errorMessage?: string
}

export interface PageInfo {
  nextCursor?: string
  hasMore?: boolean
}

export interface MessagePart {
  id: string
  text?: { text: string }
  reasoning?: { text: string }
  toolCall?: { tool: string; state?: string; inputJson?: string }
  toolResult?: { output: string; errorMessage?: string }
  attachment?: { name: string; path?: string }
}

export interface MessageInfo {
  id: string
  role: string
  parts?: MessagePart[]
  finish?: string
}

export interface Interaction {
  id: string
  session?: string
  type: string
  title: string
  detail?: string
  options?: string[]
}

export interface ModelSummary {
  id: string
  displayName?: string
  auth?: string
}

export interface ProviderSummary {
  id: string
  name?: string
  auth?: string
}

export interface CommandSummary {
  name: string
  description?: string
  argumentHint?: string
}

export interface AgentSummary {
  name: string
  model?: { providerId?: string; modelId?: string }
  hidden?: boolean
}

export interface WorkflowSummary {
  name: string
  description?: string
  stageCount?: number
}

export interface Bootstrap {
  location?: { version?: string; directory?: string }
  agents?: AgentSummary[]
  models?: ModelSummary[]
  interactions?: Interaction[]
}

export interface StreamEvent {
  seq?: string
  session?: string
  messageStarted?: { message: string; role?: string }
  messageFinished?: { message: string; finish?: string }
  partAppended?: { message: string; part: string; textDelta?: string }
  permissionRequested?: { interaction?: Interaction }
  questionRequested?: { interaction?: Interaction }
  interactionResolved?: { request?: string }
  workflowUpdated?: unknown
  sessionUpdated?: unknown
}

export interface StreamFrame {
  event?: StreamEvent
  resync?: { lastSeq?: string }
}

/** Decode SSE data blocks without assuming network chunks end on line boundaries. */
export class SseDecoder {
  private pending = ""
  private data: string[] = []

  push(chunk: string): StreamFrame[] {
    this.pending += chunk
    const frames: StreamFrame[] = []
    let end = this.pending.indexOf("\n")
    while (end >= 0) {
      const line = this.pending.slice(0, end).replace(/\r$/, "")
      this.pending = this.pending.slice(end + 1)
      if (line === "") {
        if (this.data.length > 0) {
          frames.push(JSON.parse(this.data.join("\n")) as StreamFrame)
          this.data = []
        }
      } else if (line.startsWith("data:")) {
        this.data.push(line.slice(5).replace(/^ /, ""))
      }
      end = this.pending.indexOf("\n")
    }
    return frames
  }
}

export interface ApiCommand {
  method: string
  path: string
  body?: unknown
}

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>

/** An HTTP failure with its status preserved for optional v1 capabilities. */
export class HttpError extends Error {
  constructor(readonly status: number, method: string, path: string, detail: string) {
    super(`${method} ${path}: ${detail}`)
    this.name = "HttpError"
  }
}

/** Parse one command-view request; never allow a remote URL or non-v1 path. */
export function parseApiCommand(input: string): ApiCommand {
  const match = /^\/api\s+(GET|POST|PUT|PATCH|DELETE)\s+(\/v1\/\S+)(?:\s+([\s\S]+))?$/i.exec(input.trim())
  if (!match) throw new Error("Usage: /api METHOD /v1/path [JSON object]")
  const method = match[1]?.toUpperCase() ?? ""
  const path = match[2] ?? ""
  if (method === "GET" && match[3]) throw new Error("GET requests cannot have a JSON body")
  const body = match[3] ? JSON.parse(match[3]) as unknown : undefined
  return { method, path, ...(body === undefined ? {} : { body }) }
}

export class HyaClient {
  private readonly base: string

  constructor(
    baseUrl: string,
    readonly directory: string,
    private readonly fetcher: FetchLike = fetch,
  ) {
    this.base = baseUrl.replace(/\/+$/, "")
  }

  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    if (!path.startsWith("/v1/") || path.startsWith("//")) {
      throw new Error("API path must start with /v1/")
    }
    const response = await this.fetcher(`${this.base}${path}`, {
      method,
      headers: {
        "x-hya-directory": this.directory,
        ...(body === undefined ? {} : { "content-type": "application/json" }),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    })
    if (response.headers.get("content-type")?.includes("text/event-stream")) {
      await response.body?.cancel()
      throw new Error("This endpoint streams events; open a session to view its live updates")
    }
    const bodyText = await response.text()
    let payload: unknown = null
    if (bodyText) {
      try {
        payload = JSON.parse(bodyText) as unknown
      } catch {
        if (response.ok) throw new Error(`${method} ${path}: invalid JSON response`)
      }
    }
    if (!response.ok) {
      const envelope = payload && typeof payload === "object"
        ? payload as { error?: { code?: string; message?: string } }
        : null
      const error = envelope?.error
      const detail = error?.code
        ? `${error.code}: ${error.message ?? response.statusText}`
        : `HTTP ${response.status}${response.statusText ? ` ${response.statusText}` : ""}`
      throw new HttpError(response.status, method, path, detail)
    }
    return payload as T
  }

  async createSession(agent: string, model: string, workdir: string): Promise<SessionInfo> {
    const result = await this.request<{ session: SessionInfo }>("POST", "/v1/sessions", {
      agent,
      model,
      workdir,
    })
    return result.session
  }

  async createTurn(session: string, text: string): Promise<TurnInfo> {
    const result = await this.request<{ turn: TurnInfo }>(
      "POST",
      `/v1/sessions/${encodeURIComponent(session)}/turns`,
      { prompt: { text } },
    )
    return result.turn
  }

  async createCommandTurn(session: string, command: string, argumentsText: string): Promise<TurnInfo> {
    const result = await this.request<{ turn: TurnInfo }>(
      "POST",
      `/v1/sessions/${encodeURIComponent(session)}/turns`,
      { command: { command, arguments: argumentsText } },
    )
    return result.turn
  }

  bootstrap(): Promise<Bootstrap> {
    return this.request("GET", "/v1/bootstrap")
  }

  private async listAll<T>(path: string, field: string): Promise<T[]> {
    const rows: T[] = []
    let cursor = ""
    for (let pageNumber = 0; pageNumber < 100; pageNumber++) {
      const query = `page.limit=500${cursor ? `&page.cursor=${encodeURIComponent(cursor)}` : ""}`
      const result = await this.request<Record<string, unknown> & { page?: PageInfo }>("GET", `${path}?${query}`)
      const pageRows = result[field]
      if (Array.isArray(pageRows)) rows.push(...pageRows as T[])
      if (!result.page?.hasMore) return rows
      if (!result.page.nextCursor || result.page.nextCursor === cursor) throw new Error("Server returned an invalid page cursor")
      cursor = result.page.nextCursor
    }
    throw new Error("Too many result pages")
  }

  async listSessions(): Promise<SessionInfo[]> {
    return this.listAll("/v1/sessions", "sessions")
  }

  async listMessages(session: string): Promise<MessageInfo[]> {
    return this.listAll(`/v1/sessions/${encodeURIComponent(session)}/messages`, "messages")
  }

  async listInteractions(): Promise<Interaction[]> {
    return this.listAll("/v1/interactions", "interactions")
  }

  async listModels(): Promise<ModelSummary[]> {
    return this.listAll("/v1/models", "models")
  }

  async listProviders(): Promise<ProviderSummary[]> {
    return this.listAll("/v1/providers", "providers")
  }

  async listCommands(): Promise<CommandSummary[]> {
    return this.listAll("/v1/commands", "commands")
  }

  async listSavedKeys(): Promise<string[] | null> {
    try {
      const response = await this.request<{ providerIds?: string[] }>("GET", "/v1/auth")
      return response.providerIds ?? []
    } catch (error) {
      if (error instanceof HttpError && error.status === 404) return null
      throw error
    }
  }

  async setProviderKey(provider: string, key: string): Promise<void> {
    await this.request("PUT", `/v1/auth/${encodeURIComponent(provider)}`, { apiKey: key })
  }

  async removeProviderKey(provider: string): Promise<void> {
    await this.request("DELETE", `/v1/auth/${encodeURIComponent(provider)}`)
  }

  async listWorkflows(): Promise<WorkflowSummary[]> {
    return this.listAll("/v1/workflows", "workflows")
  }

  async getWorkflowState(session: string): Promise<Record<string, unknown>> {
    return this.request("GET", `/v1/sessions/${encodeURIComponent(session)}/workflow`)
  }

  async submitWorkflow(session: string, command: Record<string, unknown>): Promise<unknown> {
    return this.request("POST", `/v1/sessions/${encodeURIComponent(session)}/workflow`, command)
  }

  async updateSessionModel(session: string, model: string): Promise<SessionInfo> {
    return this.request("PATCH", `/v1/sessions/${encodeURIComponent(session)}`, { model })
  }

  async cancelTurn(session: string, turn: string): Promise<unknown> {
    return this.request("POST", `/v1/sessions/${encodeURIComponent(session)}/turns/${encodeURIComponent(turn)}/cancel`, {})
  }

  async respondPermission(id: string, allowed: boolean): Promise<unknown> {
    return this.request("POST", `/v1/interactions/${encodeURIComponent(id)}/respond`, {
      permission: { allowed, persist: false },
    })
  }

  async respondQuestion(id: string, answer: string): Promise<unknown> {
    return this.request("POST", `/v1/interactions/${encodeURIComponent(id)}/respond`, {
      question: { answer },
    })
  }

  async listEvents(session: string, sinceSeq: string): Promise<{ events?: StreamEvent[]; nextSeq?: string }> {
    return this.request(
      "GET", `/v1/sessions/${encodeURIComponent(session)}/events?sinceSeq=${encodeURIComponent(sinceSeq)}`,
    )
  }

  async streamSession(
    session: string,
    sinceSeq: string,
    onFrame: (frame: StreamFrame) => void | Promise<void>,
    signal: AbortSignal,
  ): Promise<void> {
    const path = `/v1/sessions/${encodeURIComponent(session)}/events/stream?sinceSeq=${encodeURIComponent(sinceSeq)}`
    const response = await this.fetcher(`${this.base}${path}`, {
      headers: { "x-hya-directory": this.directory, accept: "text/event-stream" },
      signal,
    })
    if (!response.ok || !response.body) throw new Error(`Event stream: HTTP ${response.status}`)
    const reader = response.body.getReader()
    const decoder = new TextDecoder()
    const sse = new SseDecoder()
    try {
      while (!signal.aborted) {
        const { value, done } = await reader.read()
        if (done) break
        for (const frame of sse.push(decoder.decode(value, { stream: true }))) await onFrame(frame)
      }
    } finally {
      await reader.cancel().catch(() => undefined)
    }
  }
}
