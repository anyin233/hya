import type { PermissionModeInfo } from "./state/modes"
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
  /** When the session projection last changed (RFC 3339); the `/sessions` picker's relative time (state/catalog.ts). */
  timeUpdated?: string
  /** Everything billed for the session (turn rounds and side calls); the status bar's token total. */
  usage?: TokenUsage
  /** Where a forked session came from (`ForkSession`); unset for sessions that are not forks. */
  forkedFrom?: ForkSource
  /** A pending revert (`RevertSession`, `/undo`): set until `/redo` undoes it or the next prompt or shell turn commits it. */
  revert?: SessionRevert
}

/** `ForkSource`: the source session and the user message the fork was cut before (empty for a head fork). */
export interface ForkSource {
  session: string
  messageId?: string
}

/** `SessionRevert` (docs/protocol/README.md "Revert and redo"). */
export interface SessionRevert {
  /** The reverted user message (the first hidden message). */
  messageId: string
  /** Its text: `/undo` puts it back in the composer. */
  text?: string
  /** The reverted message and every later one. */
  hiddenMessages?: number
  files?: RevertedFile[]
}

/** One file a revert or redo wrote (or could not restore). */
export interface RevertedFile {
  /** Absolute path. */
  path: string
  /** `restored`, `deleted`, `unchanged`, `skipped`, or `failed`. */
  action?: string
  /** Why it was `skipped` (`too_large`, `session_cap`, `snapshot_budget`, `unreadable`) or the error of a `failed` write. */
  reason?: string
}

/**
 * `TokenUsage` (docs/protocol/README.md "Usage and context occupancy"):
 * uint64 counts as decimal strings, zero fields omitted. `input` excludes the
 * cache, so the prompt is `input + cacheRead + cacheWrite`; `output`
 * includes `reasoning`.
 */
export interface TokenUsage {
  input?: string
  output?: string
  reasoning?: string
  cacheRead?: string
  cacheWrite?: string
  reasoningUnknown?: boolean
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

/** `AttachmentPart` (docs/protocol/README.md "Prompt attachments (images)"); listings never carry `data`. */
export interface AttachmentPart {
  name: string
  mime?: string
  path?: string
  /** Bytes, a decimal string (uint64 on the wire). */
  size?: string
}

export interface MessagePart {
  id: string
  text?: { text: string }
  reasoning?: { text: string }
  toolCall?: ToolCallPart
  toolResult?: { output: string; errorMessage?: string }
  attachment?: AttachmentPart
}

/** `PromptAttachment` sent on `CreateTurn`; `data` is standard base64 of the file bytes (no `data:` prefix). */
export interface PromptAttachment {
  name: string
  mime?: string
  data: string
  path?: string
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
  /** Billed usage of an assistant message (all its rounds). */
  usage?: TokenUsage
  /** Its latest provider round alone, served by `model`: the context occupancy source. */
  roundUsage?: TokenUsage
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
  /** Provider id (`/model` picker's provider tag/group, state/catalog.ts `modelRows`). */
  providerId?: string
  /** Provider-local model id. */
  modelId?: string
  /** Context window in tokens, a decimal string; "0" or omitted when unknown. */
  contextLimit?: string
  /** Output ceiling in tokens, a decimal string; "0" or omitted when unknown. */
  outputLimit?: string
  /** The route takes reasoning effort variants (omitted = false). */
  reasoning?: boolean
  /** `false`: the model refuses image attachments (`ModelSummary.imageInput`); absent/unset means unknown, which is allowed. */
  imageInput?: boolean
  /** Where the row comes from: `remote` (model cache), `config` (config.yaml only), `override` (both; config wins per field), `offline`. */
  source?: string
}

/** `ProviderSummary` (docs/protocol/README.md "Providers and keys"); never carries a secret. */
export interface ProviderSummary {
  id: string
  name?: string
  /** `AUTH_STATUS_CREDENTIALED`, `_UNAUTHENTICATED`, `_AUTH_REJECTED`, `_AUTH_REQUIRED`, `_NOT_APPLICABLE` (offline). */
  auth?: string
  /** Last model-list outcome: `models`, `empty`, `unavailable`, `invalid`. */
  result?: string
  /** Config protocol (`openai`, `openai-response`, `anthropic`, `google`, …); empty for the offline `hya` row. */
  kind?: string
  baseUrl?: string
  /** `saved` (auth/<id>.yaml), `oauth`, `config` (inline api_key), or `none`. */
  keySource?: string
  modelCount?: number
}

/** `ProviderInfo`: a provider with its effective models. */
export interface ProviderInfo {
  summary?: ProviderSummary
  models?: ModelSummary[]
  supportsApiKey?: boolean
  supportsOauth?: boolean
}

/** A remote model-list fetch (`ProviderUpdate.discovery`); a failed fetch never fails the call. */
export interface DiscoveryOutcome {
  ok?: boolean
  /** `models`, `empty`, `auth_required`, `auth_rejected`, `unavailable`, `invalid`, `unsupported`. */
  result?: string
  /** Bounded, non-secret reason when `ok` is false. */
  errorMessage?: string
  modelCount?: number
}

/** Answer of every provider write (`UpsertProvider`, `RefreshProvider`, `SetProviderModel`, `RemoveProviderModel`). */
export interface ProviderUpdate {
  provider?: ProviderInfo
  /** Only when the call fetched the remote list. */
  discovery?: DiscoveryOutcome
}

/**
 * `SetProviderModel` body: one `models:` entry of config.yaml. The server
 * replaces `name`, `limit.context`, `limit.output`, and a boolean
 * `reasoning` with what is sent; an absent, empty, or `0` field is removed.
 */
export interface ProviderModelPatch {
  modelId: string
  displayName?: string
  /** Tokens (uint32); absent or `0` removes the limit. */
  contextLimit?: number
  outputLimit?: number
  /** Omitted: remove a boolean `reasoning`. */
  reasoning?: boolean
}

/** `TestProviderModelResponse`: `ok` false (omitted) with `errorCode` when the provider failed. */
export interface ProviderTestResponse {
  ok?: boolean
  text?: string
  /** `stop`, `length`, `tool_calls`, `cancelled`, `error`. */
  finishReason?: string
  /** `http_<status>`, `transport`, `timeout`, `unknown_model`, `incompatible`, `decode`, `auth_expired`, `provider_error`. */
  errorCode?: string
  errorMessage?: string
  latencyMs?: number | string
}

/** `SavedRule` (`GET /v1/permissions/rules`): a persisted allow/deny/ask decision. */
export interface SavedRule {
  id: string
  /** `RULE_PERMISSION_ALLOW`, `_ASK`, `_DENY`. */
  permission: string
  /** Empty matches every tool. */
  tool?: string
  pattern?: string
  timeCreated?: string
}

/** `McpServerStatus` (`GET /v1/mcp`): one configured MCP server. */
export interface McpServerStatus {
  name: string
  /** `MCP_SERVER_STATE_DESIRED`, `_CONNECTED`, `_DISCONNECTED`, `_FAILED`. */
  state?: string
  /** Namespaced tools (`mcp__server__tool`) when connected. */
  tools?: string[]
  /** Set when `state` is `_FAILED`. */
  error?: string
  authRequired?: boolean
}

/** `AgentModelSelection`: a concrete provider/model pick. */
export interface AgentModelSelection {
  providerId?: string
  modelId?: string
}

/** `AgentModelState` (`GET /v1/agent-models`): one agent's effective base model. */
export interface AgentModelState {
  agentId: string
  description?: string
  /** `primary` or `subagent`. */
  mode?: string
  hidden?: boolean
  /** Direct model/category configuration is present; such agents cannot take a remembered preference. */
  configured?: boolean
  /** Whether an automatic remembered preference can be set. */
  settable?: boolean
  /** Retained preference, including stale or configured rows. */
  preference?: AgentModelSelection
  /** Whether the retained preference exactly matches the current catalog. */
  preferenceAvailable?: boolean
  effective?: AgentModelSelection
  /** `AGENT_MODEL_SOURCE_SESSION`, `_CONFIGURED`, `_REMEMBERED`, `_DEFAULT`. */
  source?: string
  configuration?: AgentModelSelection
  sessionOverride?: AgentModelSelection
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
  /** One-line description shown in the `/agent` picker. */
  description?: string
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
  location?: { version?: string; directory?: string; pid?: number }
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
  /** Attachment parts appended after the user's text (`docs/protocol/README.md` "Prompt attachments (images)"); fold by appending parts whose id is not already on the message. */
  partsAdded?: { message: string; parts: MessagePart[] }
  errorReported?: { message?: string; code?: string; errorMessage?: string }
  /** A tool part's state; fields it does not carry are empty (fold without clearing). An empty `callId` marks a direct part overwrite. */
  toolStateChanged?: { message: string; part: string } & Partial<ToolCallPart>
  /** A member (subagent) spawn or status change on the parent session; partial frames fold by `member`. */
  memberUpdated?: MemberInfo
  permissionRequested?: { interaction?: Interaction }
  questionRequested?: { interaction?: Interaction }
  interactionResolved?: { request?: string }
  workflowUpdated?: unknown
  /** Session metadata changed; `permissionMode` is set (root session only) when the tree's mode changed. */
  sessionUpdated?: { title?: string; model?: string; agent?: string; background?: boolean; permissionMode?: string }
  /**
   * Part of the context was folded behind a summary (`docs/tui.md` "Notices"):
   * `message` is the summary system message (the divider sits right before
   * it), `foldedCount` the messages folded, `manual` a `/compact` (or
   * `/summarize`) rather than a mid-turn threshold.
   */
  compactionApplied?: { untilSeq?: string; strategy?: string; message?: string; foldedCount?: number | string; manual?: boolean }
  /** One provider call was billed (durable); `message` is empty for side calls (titles, summaries). */
  tokensRecorded?: { message?: string; model?: string; usage?: TokenUsage }
  /** The session's whole todo list after a todo tool changed it (durable). */
  todoUpdated?: { items?: TodoItem[] }
  /** A revert (`messageId` set) or its undo (`undone`, `messageId` empty) of the session (durable); re-read the session and its messages. */
  sessionReverted?: { messageId?: string; undone?: boolean; files?: RevertedFile[] }
  /**
   * Live-only, empty `session`: the provider/model catalog changed (a
   * provider added/edited/refreshed, a key set/removed, or startup discovery
   * finished). Arrives on the global stream and every session stream;
   * re-read `GET /v1/models` / `GET /v1/providers` (`docs/protocol/README.md`
   * "Live and durable frames").
   */
  catalogUpdated?: Record<string, never>
  /**
   * Live-only, empty `session`: the server is shutting down; the last frame
   * of every stream. `reason` is `stop` (`hya serve stop`: do not start
   * another), `restart` (wait for the next one), or `signal` (treat like
   * stop). A stream that ends without it lost its server unexpectedly
   * (`docs/protocol/README.md` "Server shutdown").
   */
  serverStopping?: { reason?: string }
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
  /** `detail` is the server's `code: message` (or `HTTP <status>`), without the method and path. */
  constructor(readonly status: number, method: string, path: string, readonly detail: string) {
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
  private base: string

  constructor(
    baseUrl: string,
    readonly directory: string,
    private readonly fetcher: FetchLike = fetch,
  ) {
    this.base = baseUrl.replace(/\/+$/, "")
  }

  /** The server's base URL (`/status`). */
  get baseUrl(): string { return this.base }

  /** Move every later call to another server (the database's next daemon, app/reconnect.ts). */
  setBaseUrl(baseUrl: string): void {
    this.base = baseUrl.replace(/\/+$/, "")
  }

  /** One v1 call; `signal` aborts it (the Provider View's Esc on a running call). */
  async request<T>(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
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
      ...(signal ? { signal } : {}),
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

  async createTurn(session: string, text: string, attachments?: PromptAttachment[]): Promise<TurnInfo> {
    const result = await this.request<{ turn: TurnInfo }>(
      "POST",
      `/v1/sessions/${encodeURIComponent(session)}/turns`,
      { prompt: { text, ...(attachments?.length ? { attachments } : {}) } },
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

  private async listAll<T>(path: string, field: string, extraQuery = ""): Promise<T[]> {
    const rows: T[] = []
    let cursor = ""
    for (let pageNumber = 0; pageNumber < 100; pageNumber++) {
      const query = `page.limit=500${cursor ? `&page.cursor=${encodeURIComponent(cursor)}` : ""}${extraQuery}`
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

  /** `ListAgents` (`GET /v1/agents`): the `/agent` picker's rows (state/catalog.ts `agentRows`). */
  async listAgents(): Promise<AgentSummary[]> {
    return this.listAll("/v1/agents", "agents")
  }

  async listProviders(): Promise<ProviderSummary[]> {
    return this.listAll("/v1/providers", "providers")
  }

  async listCommands(): Promise<CommandSummary[]> {
    return this.listAll("/v1/commands", "commands")
  }

  /** `SetProviderAuth` (`PUT /v1/auth/{id}`): save a key (applies live); `discovery` when the model list was fetched too. */
  async setProviderKey(provider: string, key: string, signal?: AbortSignal): Promise<{ status?: string; provider?: ProviderInfo; discovery?: DiscoveryOutcome }> {
    return this.request("PUT", `/v1/auth/${encodeURIComponent(provider)}`, { apiKey: key }, signal)
  }

  /** `RemoveProviderAuth` (`DELETE /v1/auth/{id}`): delete the saved key (applies live). */
  async removeProviderKey(provider: string, signal?: AbortSignal): Promise<void> {
    await this.request("DELETE", `/v1/auth/${encodeURIComponent(provider)}`, undefined, signal)
  }

  /** `UpsertProvider` (`PUT /v1/providers/{id}`): write kind / base URL to config.yaml, save a non-empty key, fetch the models. */
  async upsertProvider(provider: string, body: { kind: string; baseUrl: string; apiKey?: string }, signal?: AbortSignal): Promise<ProviderUpdate> {
    return this.request("PUT", `/v1/providers/${encodeURIComponent(provider)}`, body, signal)
  }

  /** `RefreshProvider` (`POST /v1/providers/{id}/refresh`): fetch the remote model list into the model cache. */
  async refreshProvider(provider: string, signal?: AbortSignal): Promise<ProviderUpdate> {
    return this.request("POST", `/v1/providers/${encodeURIComponent(provider)}/refresh`, {}, signal)
  }

  /** `SetProviderModel` (`PUT /v1/providers/{id}/models`): write one model entry into config.yaml. */
  async setProviderModel(provider: string, model: ProviderModelPatch, signal?: AbortSignal): Promise<ProviderUpdate> {
    return this.request("PUT", `/v1/providers/${encodeURIComponent(provider)}/models`, model, signal)
  }

  /** `RemoveProviderModel` (`DELETE /v1/providers/{id}/models?modelId=`): delete a model's config.yaml entry. */
  async removeProviderModel(provider: string, modelId: string, signal?: AbortSignal): Promise<ProviderUpdate> {
    return this.request("DELETE", `/v1/providers/${encodeURIComponent(provider)}/models?modelId=${encodeURIComponent(modelId)}`, undefined, signal)
  }

  /** `TestProviderModel` (`POST /v1/providers/{id}/test`): one `hi` with 1 output token (16 on Responses routes), up to 60 s. */
  async testProviderModel(provider: string, modelId: string, signal?: AbortSignal): Promise<ProviderTestResponse> {
    return this.request("POST", `/v1/providers/${encodeURIComponent(provider)}/test`, { modelId }, signal)
  }

  /** `GetVcsDiff` (`GET /v1/vcs/diff`): `git diff HEAD` plus untracked files, as one unified-diff text; `""` outside a git repo. */
  async getVcsDiff(): Promise<string> {
    const result = await this.request<{ diff?: string }>("GET", `/v1/vcs/diff?directory=${encodeURIComponent(this.directory)}`)
    return result.diff ?? ""
  }

  /** `GetMcpStatus` (`GET /v1/mcp`): every configured MCP server's status. */
  async getMcpStatus(): Promise<McpServerStatus[]> {
    const result = await this.request<{ servers?: McpServerStatus[] }>("GET", `/v1/mcp?directory=${encodeURIComponent(this.directory)}`)
    return result.servers ?? []
  }

  /** `ConnectMcp` (`POST /v1/mcp/{name}/connect`). */
  async connectMcp(name: string, signal?: AbortSignal): Promise<McpServerStatus> {
    return this.request("POST", `/v1/mcp/${encodeURIComponent(name)}/connect`, { directory: this.directory, name }, signal)
  }

  /** `DisconnectMcp` (`POST /v1/mcp/{name}/disconnect`). */
  async disconnectMcp(name: string, signal?: AbortSignal): Promise<McpServerStatus> {
    return this.request("POST", `/v1/mcp/${encodeURIComponent(name)}/disconnect`, { directory: this.directory, name }, signal)
  }

  /** `StartMcpAuth` (`POST /v1/mcp/{name}/auth`): the URL to open in a browser. */
  async startMcpAuth(name: string, signal?: AbortSignal): Promise<{ authorizationUrl?: string }> {
    return this.request("POST", `/v1/mcp/${encodeURIComponent(name)}/auth`, { directory: this.directory, name }, signal)
  }

  /** `CompleteMcpAuth` (`POST /v1/mcp/{name}/auth/complete`) with the callback code. */
  async completeMcpAuth(name: string, code: string, signal?: AbortSignal): Promise<McpServerStatus> {
    return this.request("POST", `/v1/mcp/${encodeURIComponent(name)}/auth/complete`, { directory: this.directory, name, code }, signal)
  }

  /** `RemoveMcpAuth` (`DELETE /v1/mcp/{name}/auth`): delete stored credentials. */
  async removeMcpAuth(name: string, signal?: AbortSignal): Promise<void> {
    await this.request("DELETE", `/v1/mcp/${encodeURIComponent(name)}/auth?directory=${encodeURIComponent(this.directory)}`, undefined, signal)
  }

  /** `ListSavedRules` (`GET /v1/permissions/rules`): saved permission decisions, stable id order. */
  async listSavedRules(): Promise<SavedRule[]> {
    return this.listAll("/v1/permissions/rules", "rules", `&directory=${encodeURIComponent(this.directory)}`)
  }

  /** `DeleteSavedRule` (`DELETE /v1/permissions/rules/{rule}`). */
  async deleteSavedRule(id: string, signal?: AbortSignal): Promise<void> {
    await this.request("DELETE", `/v1/permissions/rules/${encodeURIComponent(id)}?directory=${encodeURIComponent(this.directory)}`, undefined, signal)
  }

  /** `ListAgentModels` (`GET /v1/agent-models`): effective base model of every catalog agent. */
  async listAgentModels(session?: string): Promise<AgentModelState[]> {
    const query = session ? `&session=${encodeURIComponent(session)}` : ""
    const result = await this.request<{ agents?: AgentModelState[] }>("GET", `/v1/agent-models?directory=${encodeURIComponent(this.directory)}${query}`)
    return result.agents ?? []
  }

  /** `SetAgentModel` (`PUT /v1/agent-models/{agentId}`); an absent `preference` clears it. */
  async setAgentModel(agentId: string, preference: AgentModelSelection | undefined, session?: string, signal?: AbortSignal): Promise<AgentModelState> {
    return this.request("PUT", `/v1/agent-models/${encodeURIComponent(agentId)}`, {
      directory: this.directory,
      ...(session ? { session } : {}),
      ...(preference ? { preference } : {}),
    }, signal)
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

  /**
   * `UpdateSession` (`PATCH /v1/sessions/:id`) for the fields `/rename`,
   * `/agent`, and the permission mode switch change (one field per call).
   * An unknown or unavailable `permissionMode` fails with `invalid_argument`.
   */
  async updateSession(session: string, patch: { title?: string; agent?: string; permissionMode?: string }): Promise<SessionInfo> {
    return this.request("PATCH", `/v1/sessions/${encodeURIComponent(session)}`, patch)
  }

  /** `DeleteSession` (`DELETE /v1/sessions/:id`): the `/sessions` picker's delete row action (Ctrl+D). */
  async deleteSession(session: string): Promise<void> {
    await this.request("DELETE", `/v1/sessions/${encodeURIComponent(session)}`)
  }

  /** `ListPermissionModes` (`GET /v1/permission-modes`): built-ins first, then bundle modes; `[]` on a backend without the route. */
  async listPermissionModes(): Promise<PermissionModeInfo[]> {
    try {
      const result = await this.request<{ modes?: PermissionModeInfo[] }>("GET", "/v1/permission-modes")
      return result.modes ?? []
    } catch (error) {
      if (error instanceof HttpError && error.status === 404) return []
      throw error
    }
  }

  /**
   * `RevertSession` (`POST /v1/sessions/{id}/revert`): `{}` reverts the last
   * visible user message (`/undo`; again = further back), `{messageId}`
   * reverts to that user message, `{undo: true}` undoes the pending revert
   * (`/redo`). `409 session_busy` while a turn runs; `400 invalid_argument`
   * when there is nothing to revert or undo.
   */
  async revertSession(session: string, body: { messageId?: string; undo?: boolean }): Promise<{ session: SessionInfo; files?: RevertedFile[] }> {
    return this.request("POST", `/v1/sessions/${encodeURIComponent(session)}/revert`, body)
  }

  /**
   * `ForkSession` (`POST /v1/sessions/{id}/fork`): a new root session with
   * the messages strictly before user message `messageId` (its text comes
   * back as `promptText`), or every message when `messageId` is unset.
   */
  async forkSession(session: string, messageId?: string): Promise<{ session: SessionInfo; promptText?: string }> {
    return this.request("POST", `/v1/sessions/${encodeURIComponent(session)}/fork`, messageId ? { messageId } : {})
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
    /**
     * `includeDescendants=true`: also deliver the permission/question frames
     * of every session below this one (subagents), with `event.session` set
     * to the asking session (docs/protocol/README.md "Subagent asks on a
     * parent's stream"). Durable events stay per session.
     */
    includeDescendants = false,
  ): Promise<void> {
    const path = `/v1/sessions/${encodeURIComponent(session)}/events/stream?sinceSeq=${encodeURIComponent(sinceSeq)}${includeDescendants ? "&includeDescendants=true" : ""}`
    await this.readStream(path, onFrame, signal, onOpen)
  }

  /**
   * `StreamGlobalEvents` (`GET /v1/events/stream?interactionsOnly=true`):
   * every session's live interaction frames (the permission/question asks
   * and their resolves) of sessions this TUI does not have open, plus
   * `catalogUpdated`. The server leaves out every session's engine events
   * (text, tools, messages, status) and their `resync` frames
   * (`docs/protocol/README.md` "Interactions-only global stream"), so unlike
   * `streamSession` no history is replayed and no `resync` is expected.
   */
  async streamGlobal(
    onFrame: (frame: StreamFrame) => void | Promise<void>,
    signal: AbortSignal,
    /** Runs once the stream is subscribed, before any frame is read. */
    onOpen?: () => void | Promise<void>,
  ): Promise<void> {
    await this.readStream("/v1/events/stream?interactionsOnly=true", onFrame, signal, onOpen)
  }

  /** Open one SSE stream and hand every frame to `onFrame` until it ends or `signal` aborts. */
  private async readStream(
    path: string,
    onFrame: (frame: StreamFrame) => void | Promise<void>,
    signal: AbortSignal,
    onOpen?: () => void | Promise<void>,
  ): Promise<void> {
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
