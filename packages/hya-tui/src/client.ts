import type { RespondBody } from "./state/prompts"
/** Small HTTP/JSON client for the shared hya.v1 server contract. */
export interface SessionInfo {
  id: string
  agent: string
  workdir: string
  title?: string
  model?: { providerId?: string; modelId?: string; variant?: string }
  busy?: boolean
  lastSeq?: string
  /** `manual`, `yolo`, or a `<bundle-id>/<mode-id>`; empty when the server leaves it unset. */
  permissionMode?: string
  /** Parent session id when this is a subagent's session. */
  parent?: string
  /** Subagents this session spawned (folded rows; the live counterpart is `memberUpdated`). */
  members?: MemberInfo[]
}

/** A subagent (member) spawned by a session (`MemberInfo`, docs/protocol/README.md "Subagents"). */
export interface MemberInfo {
  member: string
  /** Child session id when known. */
  child?: string
  /** Subagent type (agent name). */
  agent?: string
  description?: string
  /** `MEMBER_STATUS_SPAWNING`, `_RUNNING`, `_DONE`, `_FAILED`, `_CANCELLED`. */
  status?: string
  /** Bounded finish summary. */
  summary?: string
  /** `ToolCallPart.callId` of the spawning `task` call; empty for resident spawns. */
  callId?: string
  depth?: number
}

/** `ToolCallPart` (docs/protocol/README.md "Tool calls"). */
export interface ToolCallPart {
  tool: string
  /** `TOOL_EXECUTION_STATE_PENDING` (arguments streaming), `_RUNNING`, `_OK`, `_ERROR`. */
  state?: string
  callId?: string
  /** Arguments as JSON text; empty while they still stream. */
  inputJson?: string
  /** Stored output as JSON text (`OK`). */
  outputJson?: string
  /** Wall time in ms (`OK`), a decimal string; omitted when zero. */
  durationMs?: string
  /** `ERROR`: the structured `error.type` (`unknown` when absent). */
  errorCode?: string
  errorMessage?: string
}

export interface TurnInfo {
  id: string
  state: string
  finish?: string
  errorCode?: string
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
  toolCall?: ToolCallPart
  toolResult?: { output: string; errorMessage?: string }
  attachment?: { name: string; path?: string }
}

export interface MessageError {
  code: string
  message: string
}

export interface MessageInfo {
  id: string
  role: string
  /** Agent attributed to the message; empty when the server leaves it unset. */
  agent?: string
  /** `provider/model` that produced the message; empty when unset. */
  model?: string
  parts?: MessagePart[]
  finish?: string
  finishCause?: string
  /** Why the turn that drove this assistant message failed. */
  error?: MessageError
}

export interface Interaction {
  id: string
  session?: string
  type: string
  title: string
  detail?: string
  options?: string[]
  /** Permission requests: `{action, resource, always, messageId, callId, tool, input}`; `callId` matches the waiting tool card. */
  payload?: { callId?: string; [key: string]: unknown }
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
  /** Where the command was discovered: `command` (custom/built-in) or `skill`. */
  source?: string
}

export interface TodoItem {
  id: string
  content: string
  status: string
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

/** `VcsStatus` (`GET /v1/vcs`): the status bar reads only `branch`. */
export interface VcsStatus {
  branch?: string
  head?: string
  dirty?: number
  ahead?: number
  behind?: number
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
  messageFinished?: { message: string; finish?: string; cause?: string }
  /** `tool` and `callId` are set for `kind: "tool_call"`. */
  partStarted?: { message: string; part: string; kind?: string; tool?: string; callId?: string }
  partAppended?: { message: string; part: string; textDelta?: string }
  partReplaced?: { message: string; part: string; text?: string }
  partCompleted?: { message: string; part: string }
  errorReported?: { message?: string; code?: string; errorMessage?: string }
  /** A tool part's state; fields it does not carry are empty (fold without clearing). An empty `callId` marks a direct part overwrite. */
  toolStateChanged?: { message: string; part: string } & Partial<ToolCallPart>
  /** A member (subagent) spawn or status change on the parent session; partial frames fold by `member`. */
  memberUpdated?: MemberInfo
  permissionRequested?: { interaction?: Interaction }
  questionRequested?: { interaction?: Interaction }
  interactionResolved?: { request?: string }
  workflowUpdated?: unknown
  sessionUpdated?: unknown
  /** A compaction strategy fired (`docs/tui.md` "Notices"); rendered as a transcript divider. */
  compactionApplied?: { untilSeq?: string; strategy?: string }
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

  /** The server's base URL (`/status`). */
  get baseUrl(): string { return this.base }

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

  /**
   * Run `command` as a shell turn (`ShellTurn`: the builtin shell tool, no
   * model round). The call returns when the command has finished; the
   * returned id is the shell turn's assistant message.
   */
  async createShellTurn(session: string, command: string, agent: string, model?: { providerId?: string; modelId?: string }): Promise<TurnInfo> {
    const result = await this.request<{ turn: TurnInfo }>(
      "POST",
      `/v1/sessions/${encodeURIComponent(session)}/turns`,
      { shell: { command, agent, ...(model?.providerId && model.modelId ? { model: { providerId: model.providerId, modelId: model.modelId } } : {}) } },
    )
    return result.turn
  }

  /** Relative paths under the `--dir` scope matching a glob (`FindFiles`). */
  async findFiles(pattern: string, limit: number): Promise<string[]> {
    const result = await this.request<{ paths?: string[] }>(
      "GET",
      `/v1/fs/find?pattern=${encodeURIComponent(pattern)}&limit=${limit}`,
    )
    return result.paths ?? []
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

  /** `UpdateSession` (`PATCH /v1/sessions/:id`) for the fields `/rename` and `/agent` change. */
  async updateSession(session: string, patch: { title?: string; agent?: string }): Promise<SessionInfo> {
    return this.request("PATCH", `/v1/sessions/${encodeURIComponent(session)}`, patch)
  }

  /** `CompactSession`: compact the session's context now (`/compact`). */
  async compactSession(session: string): Promise<{ compactedUntilSeq?: string; strategy?: string }> {
    return this.request("POST", `/v1/sessions/${encodeURIComponent(session)}/compact`, {})
  }

  /** `SummarizeSession`: generate a summary message for the session (`/summarize`). */
  async summarizeSession(session: string): Promise<{ summaryMessage?: string }> {
    return this.request("POST", `/v1/sessions/${encodeURIComponent(session)}/summarize`, {})
  }

  /** `GetSessionTodo` (`/todos`). */
  async getSessionTodo(session: string): Promise<TodoItem[]> {
    const result = await this.request<{ items?: TodoItem[] }>("GET", `/v1/sessions/${encodeURIComponent(session)}/todo`)
    return result.items ?? []
  }

  /** `GetVcsStatus` (`GET /v1/vcs`) scoped to the client's `--dir`; status bar git branch. */
  async getVcsStatus(): Promise<VcsStatus> {
    return this.request("GET", `/v1/vcs?directory=${encodeURIComponent(this.directory)}`)
  }

  async cancelTurn(session: string, turn: string): Promise<unknown> {
    return this.request("POST", `/v1/sessions/${encodeURIComponent(session)}/turns/${encodeURIComponent(turn)}/cancel`, {})
  }

  /** `RespondInteraction` with one response kind (state/prompts.ts `respondBody`). */
  async respondInteraction(id: string, body: RespondBody): Promise<{ applied?: boolean }> {
    return this.request("POST", `/v1/interactions/${encodeURIComponent(id)}/respond`, body)
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

  async listEvents(session: string, sinceSeq: string, limit?: number): Promise<{ events?: StreamEvent[]; nextSeq?: string }> {
    return this.request(
      "GET",
      `/v1/sessions/${encodeURIComponent(session)}/events?sinceSeq=${encodeURIComponent(sinceSeq)}${limit ? `&limit=${limit}` : ""}`,
    )
  }

  /**
   * Every durable event after `sinceSeq`, page by page, in sequence order
   * (`ListEvents` gap-fill after a stream (re)connect or `resync`).
   */
  async listEventsSince(session: string, sinceSeq: string, pageSize = 500): Promise<StreamEvent[]> {
    const events: StreamEvent[] = []
    let since = sinceSeq
    for (let pageNumber = 0; pageNumber < 1000; pageNumber++) {
      const page = await this.listEvents(session, since, pageSize)
      const rows = page.events ?? []
      events.push(...rows)
      const next = page.nextSeq ?? rows.at(-1)?.seq
      if (rows.length < pageSize || !next || BigInt(next) <= BigInt(since)) return events
      since = next
    }
    throw new Error("Too many event pages")
  }

  async streamSession(
    session: string,
    sinceSeq: string,
    onFrame: (frame: StreamFrame) => void | Promise<void>,
    signal: AbortSignal,
    /** Runs once the stream is subscribed, before any frame is read (gap-fill hook). */
    onOpen?: () => void | Promise<void>,
  ): Promise<void> {
    const path = `/v1/sessions/${encodeURIComponent(session)}/events/stream?sinceSeq=${encodeURIComponent(sinceSeq)}`
    const response = await this.fetcher(`${this.base}${path}`, {
      headers: { "x-hya-directory": this.directory, accept: "text/event-stream" },
      signal,
    })
    if (!response.ok || !response.body) throw new Error(`Event stream: HTTP ${response.status}`)
    if (onOpen) {
      try {
        await onOpen()
      } catch (error) {
        await response.body.cancel().catch(() => undefined)
        throw error
      }
    }
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
