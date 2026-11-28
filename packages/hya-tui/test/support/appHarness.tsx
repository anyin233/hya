/**
 * Reusable in-memory `testRender` UI harness for `<App />`.
 *
 * Exercises the real Solid/OpenTUI component tree, real `HyaClient`
 * serialization/SSE decoding, real `createController`, and real
 * `createStructureController` without spawning `hya serve`, SQLite, or a PTY.
 */
import type { RGBA } from "@opentui/core"
import type { KeyInput, MouseButton } from "@opentui/core/testing"
import { testRender } from "@opentui/solid"
import { createHash } from "node:crypto"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { minimumBackendVersion } from "../../frontend-version"
import { App } from "../../src/app/App"
import { AppContext } from "../../src/app/context"
import { createController, type Controller, type ControllerOptions, type TerminalAccess } from "../../src/app/controller"
import { createStructureController, type StructureControl } from "../../src/app/structure"
import {
  HyaClient,
  type AgentModelState,
  type AgentSummary,
  type Bootstrap,
  type BundleSummary,
  type CatalogDiagnostic,
  type CommandSummary,
  type FetchLike,
  type Interaction,
  type McpServerStatus,
  type MessageInfo,
  type ModelEffortPreference,
  type ModelSummary,
  type ProjectInfo,
  type ProviderInfo,
  type ProviderSummary,
  type ProviderTestResponse,
  type SavedRule,
  type SessionInfo,
  type SkillDiagnostic,
  type SkillSummary,
  type StreamEvent,
  type StreamFrame,
  type TodoItem,
  type TurnInfo,
  type VcsStatus,
  type WorkflowSummary,
} from "../../src/client"
import type { WebInfo } from "../../src/cli"
import { locateSdk } from "../../src/extensions/install"
import { extensionManager } from "../../src/extensions/manager"
import { setCustomKeybindings } from "../../src/keys/custom"
import { loadPreferences, type TuiPreferences } from "../../src/prefs"
import type { PermissionModeInfo } from "../../src/state/modes"
import { createAppStore, type AppStore } from "../../src/state/store"
import { autoThemeSetting, defaultThemeName, setTerminalThemeMode, setThemeSetting } from "../../src/theme"

export type KeyModifiers = { shift?: boolean; ctrl?: boolean; meta?: boolean; super?: boolean; hyper?: boolean }

export interface RecordedCall {
  method: string
  path: string
  body?: unknown
}

export interface FakeBackendSeed {
  version?: string
  pid?: number
  directory?: string
  projects?: ProjectInfo[]
  sessions?: SessionInfo[]
  messages?: Record<string, MessageInfo[]>
  interactions?: Interaction[]
  models?: ModelSummary[]
  agents?: AgentSummary[]
  providers?: ProviderSummary[]
  providerDetails?: Record<string, ProviderInfo>
  workflows?: WorkflowSummary[]
  workflowState?: Record<string, Record<string, unknown>>
  commands?: CommandSummary[]
  commandDiagnostics?: CatalogDiagnostic[]
  skills?: SkillSummary[]
  skillDiagnostics?: SkillDiagnostic[]
  contextFiles?: NonNullable<Bootstrap["contextFiles"]>
  permissionModes?: PermissionModeInfo[]
  todos?: Record<string, TodoItem[]>
  mcpServers?: McpServerStatus[]
  savedRules?: SavedRule[]
  agentModels?: AgentModelState[]
  modelEffortPrefs?: ModelEffortPreference[]
  bundles?: BundleSummary[]
  /** Raw rows served by `GET /v2/tui-extensions` (default `[]`). */
  tuiExtensions?: unknown[]
  /** Simulate `hya --bare` where `hya/workflow` is not loaded (workflow bundle routes return 404). */
  workflowsUnavailable?: boolean
  vcs?: VcsStatus
  diff?: string
  files?: Record<string, string | Uint8Array>
  /** When set, prompt turns automatically emit a user + assistant message with this reply. */
  autoReply?: string | ((text: string, sessionId: string) => string | { text: string; reasoning?: string; finish?: string })
  onTurn?: (sessionId: string, body: Record<string, unknown>, backend: FakeBackend) => TurnInfo | Promise<TurnInfo>
  onRespond?: (id: string, body: Record<string, unknown>, backend: FakeBackend) => void | Promise<void>
  onTestProviderModel?: (provider: string, modelId: string) => ProviderTestResponse | Promise<ProviderTestResponse>
  onRequest?: (method: string, path: string, body: unknown, backend: FakeBackend) => Response | undefined | Promise<Response | undefined>
}

interface StreamSink {
  push(frame: StreamFrame): void
  close(): void
}

const encoder = new TextEncoder()

function jsonResponse(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), { status, headers: { "content-type": "application/json" } })
}

export function errorResponse(status: number, code: string, message: string): Response {
  return jsonResponse({ error: { code, message } }, status)
}

function makeSseResponse(register: (sink: StreamSink) => () => void, signal?: AbortSignal): Response {
  let cleanup: (() => void) | undefined
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      let closed = false
      const close = () => {
        if (closed) return
        closed = true
        cleanup?.()
        cleanup = undefined
        try { controller.close() } catch { /* already closed */ }
      }
      const sink: StreamSink = {
        push(frame) {
          if (closed) return
          try { controller.enqueue(encoder.encode(`data: ${JSON.stringify(frame)}\n\n`)) } catch { close() }
        },
        close,
      }
      cleanup = register(sink)
      if (signal) {
        if (signal.aborted) close()
        else signal.addEventListener("abort", close, { once: true })
      }
    },
    cancel() {
      cleanup?.()
      cleanup = undefined
    },
  })
  return new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } })
}

function globMatch(path: string, pattern: string): boolean {
  if (!pattern || pattern === "*" || pattern === "**/*") return true
  const normalized = pattern.replace(/^\*\*\//, "")
  const escaped = normalized.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*\*/g, "§§").replace(/\*/g, "[^/]*").replace(/§§/g, ".*")
  const needle = normalized.replace(/\*/g, "").toLowerCase()
  return new RegExp(`^${escaped}$`, "i").test(path) || (needle.length > 0 && path.toLowerCase().includes(needle))
}

export class FakeBackend {
  version: string
  pid: number
  directory: string
  projects: ProjectInfo[]
  sessions: SessionInfo[]
  messages = new Map<string, MessageInfo[]>()
  interactions: Interaction[]
  models: ModelSummary[]
  agents: AgentSummary[]
  providers: ProviderSummary[]
  providerDetails = new Map<string, ProviderInfo>()
  workflows: WorkflowSummary[]
  workflowState = new Map<string, Record<string, unknown>>()
  commands: CommandSummary[]
  commandDiagnostics: CatalogDiagnostic[]
  skills: SkillSummary[]
  skillDiagnostics: SkillDiagnostic[]
  contextFiles: NonNullable<Bootstrap["contextFiles"]>
  permissionModes: PermissionModeInfo[]
  todos = new Map<string, TodoItem[]>()
  mcpServers: McpServerStatus[]
  savedRules: SavedRule[]
  agentModels: AgentModelState[]
  modelEffortPrefs: ModelEffortPreference[]
  bundles: BundleSummary[]
  tuiExtensions: unknown[]
  workflowsUnavailable: boolean
  vcs: VcsStatus
  diff: string
  files = new Map<string, Uint8Array>()
  calls: RecordedCall[] = []
  private readonly seed: FakeBackendSeed
  private sessionStreams = new Map<string, Set<StreamSink>>()
  private globalStreams = new Set<StreamSink>()
  private seq = 10
  private sessionCounter = 0
  private messageCounter = 0

  constructor(seed: FakeBackendSeed = {}) {
    this.seed = seed
    this.version = seed.version ?? minimumBackendVersion
    this.pid = seed.pid ?? 4242
    this.directory = seed.directory ?? "/workspace"
    this.projects = seed.projects ? structuredClone(seed.projects) : [
      { id: "prj_work", name: "workspace", roots: [this.directory], sessionCount: seed.sessions?.length ?? 0, busy: false },
    ]
    this.sessions = seed.sessions ? structuredClone(seed.sessions) : []
    for (const s of this.sessions) {
      const m = /^hysec_(\d+)$/.exec(s.id)
      if (m) this.sessionCounter = Math.max(this.sessionCounter, Number(m[1]))
    }
    for (const [id, list] of Object.entries(seed.messages ?? {})) this.messages.set(id, structuredClone(list))
    this.interactions = seed.interactions ? structuredClone(seed.interactions) : []
    this.models = seed.models ? structuredClone(seed.models) : [
      { id: "fake/model", providerId: "fake", modelId: "model", displayName: "Fake Model", contextLimit: "100000", reasoning: true, reasoningVariants: ["none", "low", "medium", "high"], source: "config" },
      { id: "fake/beta", providerId: "fake", modelId: "beta", displayName: "Fake Beta", contextLimit: "100000", source: "config" },
      { id: "hya/offline", providerId: "hya", modelId: "offline", source: "offline" },
    ]
    this.agents = seed.agents ? structuredClone(seed.agents) : [
      { name: "hya-main", description: "Main coding agent", model: { providerId: "fake", modelId: "model" } },
      { name: "hya-plan", description: "Planning agent" },
      { name: "hya-task", description: "Subagent task runner" },
    ]
    this.providers = seed.providers ? structuredClone(seed.providers) : [
      { id: "fake", kind: "openai-compatible", baseUrl: "http://127.0.0.1:1/v1", auth: "AUTH_STATUS_CREDENTIALED", keySource: "config", modelCount: 2, result: "models" },
      { id: "hya", auth: "AUTH_STATUS_NOT_APPLICABLE", keySource: "none", modelCount: 1 },
    ]
    for (const [id, info] of Object.entries(seed.providerDetails ?? {})) this.providerDetails.set(id, structuredClone(info))
    this.workflows = seed.workflows ? structuredClone(seed.workflows) : []
    for (const [id, st] of Object.entries(seed.workflowState ?? {})) this.workflowState.set(id, structuredClone(st))
    this.commands = seed.commands ? structuredClone(seed.commands) : []
    this.commandDiagnostics = seed.commandDiagnostics ? structuredClone(seed.commandDiagnostics) : []
    this.skills = seed.skills ? structuredClone(seed.skills) : []
    this.skillDiagnostics = seed.skillDiagnostics ? structuredClone(seed.skillDiagnostics) : []
    this.contextFiles = seed.contextFiles ? structuredClone(seed.contextFiles) : []
    this.permissionModes = seed.permissionModes ? structuredClone(seed.permissionModes) : [
      { id: "manual", title: "Manual", source: "builtin", description: "Ask before shell or writes" },
      { id: "yolo", title: "Yolo", source: "builtin", description: "Bypass all permission checks" },
    ]
    for (const [id, list] of Object.entries(seed.todos ?? {})) this.todos.set(id, structuredClone(list))
    this.mcpServers = seed.mcpServers ? structuredClone(seed.mcpServers) : []
    this.savedRules = seed.savedRules ? structuredClone(seed.savedRules) : []
    this.agentModels = seed.agentModels ? structuredClone(seed.agentModels) : [
      { agentId: "hya-main", mode: "primary", description: "Main coding agent", settable: true, effective: { providerId: "fake", modelId: "model" }, source: "AGENT_MODEL_SOURCE_DEFAULT", effortSource: "AGENT_EFFORT_SOURCE_NONE" },
      { agentId: "hya-plan", mode: "primary", description: "Planning agent", settable: true, effective: { providerId: "fake", modelId: "model" }, source: "AGENT_MODEL_SOURCE_DEFAULT", effortSource: "AGENT_EFFORT_SOURCE_NONE" },
      { agentId: "hya-task", mode: "subagent", description: "Subagent task runner", settable: true, effective: { providerId: "fake", modelId: "model" }, source: "AGENT_MODEL_SOURCE_DEFAULT", effortSource: "AGENT_EFFORT_SOURCE_NONE" },
    ]
    this.modelEffortPrefs = seed.modelEffortPrefs ? structuredClone(seed.modelEffortPrefs) : []
    this.bundles = seed.bundles ? structuredClone(seed.bundles) : []
    this.tuiExtensions = seed.tuiExtensions ? structuredClone(seed.tuiExtensions) : []
    this.workflowsUnavailable = seed.workflowsUnavailable === true
    this.vcs = seed.vcs ? structuredClone(seed.vcs) : { branch: "main" }
    this.diff = seed.diff ?? ""
    for (const [path, data] of Object.entries(seed.files ?? {})) {
      this.files.set(path, typeof data === "string" ? encoder.encode(data) : data)
    }
  }

  nextSeq(): string {
    return String(++this.seq)
  }

  emitSession(sessionId: string, event: Omit<StreamEvent, "session"> & { session?: string }): void {
    const seq = event.seq ?? this.nextSeq()
    const full: StreamEvent = { ...event, seq, session: event.session ?? sessionId }
    const session = this.sessions.find((s) => s.id === sessionId)
    if (session && seq !== "0") session.lastSeq = seq
    for (const sink of this.sessionStreams.get(sessionId) ?? []) sink.push({ event: full })
  }

  emitSessionFrame(sessionId: string, frame: StreamFrame): void {
    for (const sink of this.sessionStreams.get(sessionId) ?? []) sink.push(frame)
  }

  emitGlobal(event: StreamEvent): void {
    for (const sink of this.globalStreams) sink.push({ event })
  }

  closeAllStreams(): void {
    for (const set of this.sessionStreams.values()) for (const sink of set) sink.close()
    this.sessionStreams.clear()
    for (const sink of this.globalStreams) sink.close()
    this.globalStreams.clear()
  }

  /** Append a completed user+assistant exchange to `sessionId` and notify streams. */
  emitReply(sessionId: string, userText: string, reply: { text: string; reasoning?: string; finish?: string; model?: string; agent?: string }): void {
    const session = this.sessions.find((s) => s.id === sessionId)
    const userId = `msg_u${++this.messageCounter}`
    const asstId = `msg_a${++this.messageCounter}`
    const model = reply.model ?? (session?.model?.providerId && session.model.modelId ? `${session.model.providerId}/${session.model.modelId}` : "fake/model")
    const agent = reply.agent ?? session?.agent ?? "hya-main"
    const finish = reply.finish ?? "FINISH_REASON_STOP"
    const parts: NonNullable<MessageInfo["parts"]> = []
    if (reply.reasoning !== undefined) parts.push({ id: `${asstId}_r`, reasoning: { text: reply.reasoning } })
    parts.push({ id: `${asstId}_t`, text: { text: reply.text } })
    const list = this.messages.get(sessionId) ?? []
    list.push(
      { id: userId, role: "ROLE_USER", parts: [{ id: `${userId}_t`, text: { text: userText } }], finish: "FINISH_REASON_STOP" },
      { id: asstId, role: "ROLE_ASSISTANT", agent, model, parts, finish },
    )
    this.messages.set(sessionId, list)
    this.emitSession(sessionId, { messageStarted: { message: userId, role: "ROLE_USER" } })
    this.emitSession(sessionId, { partStarted: { message: userId, part: `${userId}_t`, kind: "text" } })
    this.emitSession(sessionId, { partAppended: { message: userId, part: `${userId}_t`, textDelta: userText } })
    this.emitSession(sessionId, { messageFinished: { message: userId, finish: "FINISH_REASON_STOP" } })
    this.emitSession(sessionId, { messageStarted: { message: asstId, role: "ROLE_ASSISTANT" } })
    if (reply.reasoning !== undefined) {
      this.emitSession(sessionId, { partStarted: { message: asstId, part: `${asstId}_r`, kind: "reasoning" } })
      this.emitSession(sessionId, { partAppended: { message: asstId, part: `${asstId}_r`, textDelta: reply.reasoning } })
    }
    this.emitSession(sessionId, { partStarted: { message: asstId, part: `${asstId}_t`, kind: "text" } })
    this.emitSession(sessionId, { partAppended: { message: asstId, part: `${asstId}_t`, textDelta: reply.text } })
    this.emitSession(sessionId, { messageFinished: { message: asstId, finish } })
  }

  fetcher: FetchLike = async (input: string, init?: RequestInit): Promise<Response> => {
    const url = new URL(input)
    const method = (init?.method ?? "GET").toUpperCase()
    const pathname = url.pathname
    const bodyText = typeof init?.body === "string" ? init.body : undefined
    const body = bodyText ? JSON.parse(bodyText) as Record<string, unknown> : undefined
    const pathWithQuery = `${pathname}${url.search}`
    this.calls.push({ method, path: pathWithQuery, ...(body === undefined ? {} : { body }) })

    if (this.seed.onRequest) {
      const override = await this.seed.onRequest(method, pathWithQuery, body, this)
      if (override) return override
    }

    // SSE streams
    if (method === "GET" && pathname === "/v2/events/stream") {
      return makeSseResponse((sink) => {
        this.globalStreams.add(sink)
        return () => this.globalStreams.delete(sink)
      }, init?.signal ?? undefined)
    }
    const streamMatch = /^\/v2\/sessions\/([^/]+)\/events\/stream$/.exec(pathname)
    if (method === "GET" && streamMatch) {
      const id = decodeURIComponent(streamMatch[1]!)
      return makeSseResponse((sink) => {
        let set = this.sessionStreams.get(id)
        if (!set) { set = new Set(); this.sessionStreams.set(id, set) }
        set.add(sink)
        return () => set?.delete(sink)
      }, init?.signal ?? undefined)
    }

    if (method === "GET" && pathname === "/v2/health") return jsonResponse({ ok: true })
    if (method === "GET" && pathname === "/v2/bootstrap") {
      return jsonResponse({
        location: { version: this.version, directory: this.directory, pid: this.pid },
        agents: this.agents,
        models: this.models,
        interactions: this.interactions,
        skills: this.skills,
        commands: this.commands,
        contextFiles: this.contextFiles,
      })
    }

    // Projects
    if (method === "POST" && pathname === "/v2/projects/ensure") {
      const path = String(body?.path ?? this.directory)
      let project = this.projects.find((p) => p.roots.some((r) => path === r || path.startsWith(`${r}/`)))
      let created = false
      if (!project) {
        project = { id: `prj_${this.projects.length + 1}`, name: path.split("/").filter(Boolean).at(-1) ?? "project", roots: [path], sessionCount: 0, busy: false }
        this.projects.push(project)
        created = true
      }
      return jsonResponse({ project, created })
    }
    if (method === "GET" && pathname === "/v2/projects/resolve") {
      const path = url.searchParams.get("path") ?? ""
      const project = this.projects.find((p) => p.roots.some((r) => path === r || path.startsWith(`${r}/`)))
      return jsonResponse({ ...(project ? { project } : {}) })
    }
    if (method === "GET" && pathname === "/v2/projects") return jsonResponse({ projects: this.projects })
    if (method === "POST" && pathname === "/v2/projects") {
      const project: ProjectInfo = { id: `prj_${this.projects.length + 1}`, name: String(body?.name ?? "project"), roots: (body?.roots as string[]) ?? [], sessionCount: 0, busy: false }
      this.projects.push(project)
      return jsonResponse(project)
    }
    const projectMatch = /^\/v2\/projects\/([^/]+)$/.exec(pathname)
    if (projectMatch) {
      const id = decodeURIComponent(projectMatch[1]!)
      const index = this.projects.findIndex((p) => p.id === id)
      if (index < 0) return errorResponse(404, "not_found", `project ${id} not found`)
      if (method === "GET") return jsonResponse(this.projects[index])
      if (method === "PATCH") {
        const next = { ...this.projects[index]!, ...(body?.name !== undefined ? { name: String(body.name) } : {}), ...(Array.isArray(body?.roots) ? { roots: body.roots as string[] } : {}) }
        this.projects[index] = next
        return jsonResponse(next)
      }
      if (method === "DELETE") {
        if (this.sessions.some((s) => s.projectId === id && !s.archived)) return errorResponse(409, "failed_precondition", "project has live sessions")
        this.projects.splice(index, 1)
        return jsonResponse({})
      }
    }

    // Sessions
    if (method === "GET" && pathname === "/v2/sessions") {
      const projectId = url.searchParams.get("projectId")
      const includeArchived = url.searchParams.get("includeArchived") === "true"
      const rows = this.sessions.filter((s) => (!projectId || s.projectId === projectId) && (includeArchived || !s.archived))
      return jsonResponse({ sessions: rows })
    }
    if (method === "POST" && pathname === "/v2/sessions") {
      const id = `hysec_${++this.sessionCounter}`
      const modelStr = typeof body?.model === "string" && body.model ? body.model : "fake/model"
      const [providerId, ...rest] = modelStr.split("/")
      const modelPart = rest.join("/")
      const [modelId, variant] = modelPart.includes("#") ? modelPart.split("#") : [modelPart, undefined]
      const session: SessionInfo = {
        id,
        agent: typeof body?.agent === "string" && body.agent ? body.agent : (this.agents[0]?.name ?? "hya-main"),
        workdir: String(body?.workdir ?? (body?.kind === "SESSION_KIND_TEMPORARY" ? "/tmp/scratch" : this.directory)),
        model: providerId && modelId ? { providerId, modelId, ...(variant ? { variant } : {}) } : { providerId: "fake", modelId: "model" },
        ...(body?.projectId ? { projectId: String(body.projectId) } : {}),
        kind: (body?.kind as SessionInfo["kind"]) ?? "SESSION_KIND_PROJECT",
        ...(body?.ephemeral ? { ephemeral: true } : {}),
        permissionMode: "manual",
        lastSeq: "0",
        timeCreated: new Date().toISOString(),
        timeUpdated: new Date().toISOString(),
      }
      this.sessions.push(session)
      this.messages.set(id, [])
      return jsonResponse({ session })
    }

    const sessionAction = /^\/v2\/sessions\/([^/]+)(?:\/(.*))?$/.exec(pathname)
    if (sessionAction) {
      const id = decodeURIComponent(sessionAction[1]!)
      const tail = sessionAction[2] ?? ""
      const session = this.sessions.find((s) => s.id === id)
      if (!session) return errorResponse(404, "not_found", `session ${id} not found`)
      if (tail === "") {
        if (method === "GET") return jsonResponse(session)
        if (method === "PATCH") {
          if (typeof body?.title === "string") session.title = body.title
          if (typeof body?.agent === "string") session.agent = body.agent
          if (typeof body?.permissionMode === "string") session.permissionMode = body.permissionMode
          if (typeof body?.archived === "boolean") session.archived = body.archived
          if (typeof body?.model === "string") {
            const [providerId, ...rest] = body.model.split("/")
            const full = rest.join("/")
            const [modelId, variant] = full.includes("#") ? full.split("#") : [full, undefined]
            session.model = { providerId, modelId, ...(variant ? { variant } : {}) }
            session.effectiveEffort = variant ?? "default"
            session.effortSource = variant ? "session" : "default"
          }
          return jsonResponse(session)
        }
        if (method === "DELETE") {
          this.sessions = this.sessions.filter((s) => s.id !== id)
          this.messages.delete(id)
          return jsonResponse({})
        }
      }
      if (tail === "messages" && method === "GET") return jsonResponse({ messages: this.messages.get(id) ?? [] })
      if (tail === "events" && method === "GET") return jsonResponse({ events: [], nextSeq: session.lastSeq ?? "0" })
      if (tail === "todo" && method === "GET") return jsonResponse({ items: this.todos.get(id) ?? [] })
      if ((tail === "workflow" || tail === "bundles/hya%2Fworkflow/state") && method === "GET") return jsonResponse(this.workflowState.get(id) ?? {})
      if ((tail === "workflow" || tail === "bundles/hya%2Fworkflow/command") && method === "POST") return jsonResponse({ ok: true })
      if (tail === "turns" && method === "POST") {
        session.ephemeral = false
        if (this.seed.onTurn) return jsonResponse({ turn: await this.seed.onTurn(id, body ?? {}, this) })
        const prompt = body?.prompt as { text?: string } | undefined
        const shell = body?.shell as { command?: string } | undefined
        const cmd = body?.command as { command?: string; arguments?: string } | undefined
        if (shell) {
          const userId = `msg_shell_u_${++this.messageCounter}`
          const msgId = `msg_shell_${++this.messageCounter}`
          const inputJson = JSON.stringify({ command: shell.command })
          const outputJson = JSON.stringify({ stdout: `${shell.command}\n` })
          const list = this.messages.get(id) ?? []
          list.push(
            { id: userId, role: "ROLE_USER", parts: [{ id: `${userId}_t`, text: { text: "The following tool was executed by the user" } }], finish: "FINISH_REASON_STOP" },
            {
              id: msgId, role: "ROLE_ASSISTANT", agent: session.agent,
              parts: [{ id: `${msgId}_p`, toolCall: { tool: "bash", state: "TOOL_EXECUTION_STATE_OK", inputJson, outputJson, durationMs: "5" } }],
              finish: "FINISH_REASON_STOP",
            },
          )
          this.messages.set(id, list)
          this.emitSession(id, { messageStarted: { message: userId, role: "ROLE_USER" } })
          this.emitSession(id, { partStarted: { message: userId, part: `${userId}_t`, kind: "text" } })
          this.emitSession(id, { partAppended: { message: userId, part: `${userId}_t`, textDelta: "The following tool was executed by the user" } })
          this.emitSession(id, { messageFinished: { message: userId, finish: "FINISH_REASON_STOP" } })
          this.emitSession(id, { messageStarted: { message: msgId, role: "ROLE_ASSISTANT" } })
          this.emitSession(id, { partStarted: { message: msgId, part: `${msgId}_p`, kind: "tool_call", tool: "bash" } })
          this.emitSession(id, { toolStateChanged: { message: msgId, part: `${msgId}_p`, tool: "bash", state: "TOOL_EXECUTION_STATE_OK", inputJson, outputJson, durationMs: "5" } })
          this.emitSession(id, { messageFinished: { message: msgId, finish: "FINISH_REASON_STOP" } })
          return jsonResponse({ turn: { id: msgId, state: "TURN_STATE_FINISHED", finish: "FINISH_REASON_STOP" } })
        }
        const turnId = `msg_u${++this.messageCounter}`
        const text = prompt?.text ?? (cmd ? `/${cmd.command}${cmd.arguments ? ` ${cmd.arguments}` : ""}` : "")
        if (this.seed.autoReply !== undefined) {
          const out = typeof this.seed.autoReply === "function" ? this.seed.autoReply(text, id) : this.seed.autoReply
          const replyObj = typeof out === "string" ? { text: out } : out
          queueMicrotask(() => this.emitReply(id, text, replyObj))
        }
        return jsonResponse({ turn: { id: turnId, state: "TURN_STATE_RUNNING" } })
      }
      const cancelMatch = /^turns\/([^/]+)\/cancel$/.exec(tail)
      if (cancelMatch && method === "POST") {
        const msgId = `msg_cancel_${++this.messageCounter}`
        const list = this.messages.get(id) ?? []
        list.push({ id: msgId, role: "ROLE_ASSISTANT", agent: session.agent, parts: [], finish: "FINISH_REASON_CANCELLED" })
        this.messages.set(id, list)
        this.emitSession(id, { messageStarted: { message: msgId, role: "ROLE_ASSISTANT" } })
        this.emitSession(id, { messageFinished: { message: msgId, finish: "FINISH_REASON_CANCELLED" } })
        return jsonResponse({})
      }
      if (tail === "revert" && method === "POST") {
        const list = this.messages.get(id) ?? []
        if (body?.undo) {
          session.revert = undefined
          return jsonResponse({ session, files: [] })
        }
        const target = body?.messageId
          ? list.find((m) => m.id === body.messageId)
          : [...list].reverse().find((m) => m.role === "ROLE_USER")
        if (!target) return errorResponse(400, "invalid_argument", "nothing to revert")
        const idx = list.indexOf(target)
        const text = target.parts?.map((p) => p.text?.text ?? "").join("") ?? ""
        session.revert = { messageId: target.id, text, hiddenMessages: list.length - idx, files: [] }
        return jsonResponse({ session, files: [] })
      }
      if (tail === "fork" && method === "POST") {
        const list = this.messages.get(id) ?? []
        const cut = body?.messageId ? list.findIndex((m) => m.id === body.messageId) : list.length
        const target = cut >= 0 && cut < list.length ? list[cut] : undefined
        const promptText = target?.parts?.map((p) => p.text?.text ?? "").join("") ?? ""
        const forked: SessionInfo = {
          ...structuredClone(session),
          id: `hysec_${++this.sessionCounter}`,
          title: undefined,
          forkedFrom: { session: id, ...(body?.messageId ? { messageId: String(body.messageId) } : {}) },
        }
        this.sessions.push(forked)
        this.messages.set(forked.id, structuredClone(list.slice(0, cut < 0 ? list.length : cut)))
        return jsonResponse({ session: forked, ...(promptText ? { promptText } : {}) })
      }
      if (tail === "compact" && method === "POST") {
        const seq = this.nextSeq()
        this.emitSession(id, { seq, compactionApplied: { untilSeq: seq, strategy: "rewrite", foldedCount: 2, manual: true } })
        return jsonResponse({ compactedUntilSeq: seq, strategy: "rewrite" })
      }
      if (tail === "summarize" && method === "POST") return jsonResponse({ summaryMessage: "msg_sum" })
      if (tail === "refresh-context" && method === "POST") return jsonResponse({ pending: true })
      if ((tail === "archive" || tail === "bundles/hya%2Fmulti-agent/archive") && method === "POST") {
        const parent = this.sessions.find((s) => s.members?.some((m) => m.child === id))
        const member = parent?.members?.find((m) => m.child === id)
        if (member) member.archived = true
        if (parent) this.emitSession(parent.id, { memberLifecycleChanged: { child: id, ...(member?.handle ? { handle: member.handle } : {}), archived: true, archiveReason: "user" } })
        return jsonResponse({ handle: member?.handle ?? id, cancelledTurn: false, descendants: [] })
      }
      if ((tail === "steer" || tail === "bundles/hya%2Fmulti-agent/steer") && method === "POST") {
        const parent = this.sessions.find((s) => s.members?.some((m) => m.child === id))
        const member = parent?.members?.find((m) => m.child === id)
        if (member && member.archived) {
          member.archived = false
          if (parent) this.emitSession(parent.id, { memberLifecycleChanged: { child: id, ...(member.handle ? { handle: member.handle } : {}), archived: false } })
        }
        return jsonResponse({ from: "user" })
      }
    }

    // Interactions
    if (method === "GET" && pathname === "/v2/interactions") return jsonResponse({ interactions: this.interactions })
    const respondMatch = /^\/v2\/interactions\/([^/]+)\/respond$/.exec(pathname)
    if (method === "POST" && respondMatch) {
      const id = decodeURIComponent(respondMatch[1]!)
      const existing = this.interactions.find((i) => i.id === id)
      this.interactions = this.interactions.filter((i) => i.id !== id)
      await this.seed.onRespond?.(id, body ?? {}, this)
      if (existing?.session) this.emitSession(existing.session, { interactionResolved: { request: id } })
      this.emitGlobal({ interactionResolved: { request: id } })
      return jsonResponse({ applied: true })
    }

    // Catalogs & views
    if (method === "GET" && pathname === "/v2/models") return jsonResponse({ models: this.models })
    if (method === "GET" && pathname === "/v2/agents") return jsonResponse({ agents: this.agents })
    if (method === "GET" && (pathname === "/v2/workflows" || pathname === "/v2/bundles/hya%2Fworkflow/api/workflows")) {
      if (this.workflowsUnavailable) return errorResponse(404, "not_found", "bundle hya/workflow is not loaded")
      return jsonResponse({ workflows: this.workflows })
    }
    if (method === "GET" && pathname === "/v2/providers") return jsonResponse({ providers: this.providers })
    if (method === "GET" && pathname === "/v2/commands") return jsonResponse({ commands: this.commands, diagnostics: this.commandDiagnostics })
    if (method === "GET" && pathname === "/v2/skills") return jsonResponse({ skills: this.skills, diagnostics: this.skillDiagnostics })
    if (method === "GET" && pathname === "/v2/permission-modes") return jsonResponse({ modes: this.permissionModes })
    if (method === "GET" && pathname === "/v2/tui-extensions") return jsonResponse({ extensions: this.tuiExtensions })
    if (method === "GET" && pathname === "/v2/vcs") return jsonResponse(this.vcs)
    if (method === "GET" && pathname === "/v2/vcs/diff") return jsonResponse({ diff: this.diff })

    // Providers write/test
    const authMatch = /^\/v2\/auth\/([^/]+)$/.exec(pathname)
    if (authMatch) {
      const id = decodeURIComponent(authMatch[1]!)
      const row = this.providers.find((p) => p.id === id)
      if (method === "PUT") {
        if (row) { row.keySource = "saved"; row.auth = "AUTH_STATUS_CREDENTIALED" }
        return jsonResponse({ status: "saved", discovery: { ok: true, result: "models", modelCount: row?.modelCount ?? 0 } })
      }
      if (method === "DELETE") {
        if (row) { row.keySource = "none"; row.auth = "AUTH_STATUS_UNAUTHENTICATED" }
        return jsonResponse({})
      }
    }
    const providerMatch = /^\/v2\/providers\/([^/]+)(?:\/(.*))?$/.exec(pathname)
    if (providerMatch) {
      const id = decodeURIComponent(providerMatch[1]!)
      const tail = providerMatch[2] ?? ""
      if (tail === "" && method === "PUT") {
        const existing = this.providers.find((p) => p.id === id)
        const next: ProviderSummary = {
          id, kind: String(body?.kind ?? "openai"), baseUrl: String(body?.baseUrl ?? ""),
          keySource: body?.apiKey ? "saved" : "none", auth: body?.apiKey ? "AUTH_STATUS_CREDENTIALED" : "AUTH_STATUS_UNAUTHENTICATED",
          modelCount: existing?.modelCount ?? 1, result: "models",
        }
        if (existing) Object.assign(existing, next)
        else this.providers.unshift(next)
        if (!this.models.some((m) => m.providerId === id)) {
          this.models.push({ id: `${id}/alpha`, providerId: id, modelId: "alpha", source: "remote" })
        }
        return jsonResponse({ discovery: { ok: true, result: "models", modelCount: this.models.filter((m) => m.providerId === id).length } })
      }
      if (tail === "refresh" && method === "POST") {
        return jsonResponse({ discovery: { ok: true, result: "models", modelCount: this.models.filter((m) => m.providerId === id).length } })
      }
      if (tail === "models" && method === "PUT") {
        const modelId = String(body?.modelId ?? "")
        const fullId = `${id}/${modelId}`
        const existing = this.models.find((m) => m.id === fullId)
        const patch: Partial<ModelSummary> = {
          ...(body?.displayName ? { displayName: String(body.displayName) } : {}),
          ...(body?.contextLimit ? { contextLimit: String(body.contextLimit) } : {}),
          ...(body?.outputLimit ? { outputLimit: String(body.outputLimit) } : {}),
          ...(typeof body?.reasoning === "boolean" ? { reasoning: body.reasoning } : {}),
        }
        if (existing) { Object.assign(existing, patch); existing.source = existing.source === "remote" ? "override" : "config" }
        else this.models.push({ id: fullId, providerId: id, modelId, source: "config", ...patch })
        return jsonResponse({})
      }
      if (tail === "models" && method === "DELETE") {
        const modelId = url.searchParams.get("modelId") ?? ""
        this.models = this.models.filter((m) => m.id !== `${id}/${modelId}`)
        return jsonResponse({})
      }
      if (tail === "test" && method === "POST") {
        const modelId = String(body?.modelId ?? "")
        if (this.seed.onTestProviderModel) return jsonResponse(await this.seed.onTestProviderModel(id, modelId))
        return jsonResponse({ ok: true, text: "hi", finishReason: "stop", latencyMs: 12 })
      }
    }

    // Bundles
    if (method === "GET" && pathname === "/v2/bundles") return jsonResponse({ bundles: this.bundles })
    if (method === "POST" && pathname === "/v2/bundles:refresh") return jsonResponse({ bundles: this.bundles, errors: [] })
    if (method === "POST" && pathname === "/v2/bundles:install") {
      const path = String(body?.path ?? "")
      const id = path.split("/").at(-1)?.replace(/\.hyabundle$/, "") || "user/bundle"
      this.bundles.push({ id: `e2e/${id}`, version: "1.0.0", scope: body?.project ? "project" : "user", state: "active", enabled: true, removable: true })
      return jsonResponse({ refresh: { bundles: this.bundles, errors: [] } })
    }
    if (method === "POST" && pathname === "/v2/bundles:uninstall") {
      const id = String(body?.bundleId ?? "")
      this.bundles = this.bundles.filter((b) => b.id !== id)
      return jsonResponse({})
    }
    if (method === "POST" && pathname === "/v2/bundles:set-enabled") {
      const id = String(body?.bundleId ?? "")
      const enabled = body?.enabled === true
      for (const b of this.bundles) if (b.id === id) { b.enabled = enabled; b.state = enabled ? "active" : "disabled" }
      return jsonResponse({})
    }

    // MCP
    if (method === "GET" && pathname === "/v2/mcp") return jsonResponse({ servers: this.mcpServers })
    const mcpMatch = /^\/v2\/mcp\/([^/]+)\/(connect|disconnect|auth|auth\/complete)$/.exec(pathname)
    if (mcpMatch) {
      const name = decodeURIComponent(mcpMatch[1]!)
      const row = this.mcpServers.find((s) => s.name === name) ?? { name }
      if (mcpMatch[2] === "connect") { row.state = "MCP_SERVER_STATE_CONNECTED"; return jsonResponse(row) }
      if (mcpMatch[2] === "disconnect") { row.state = "MCP_SERVER_STATE_DISCONNECTED"; return jsonResponse(row) }
      return jsonResponse(row)
    }

    // Saved rules
    if (method === "GET" && pathname === "/v2/permissions/rules") return jsonResponse({ rules: this.savedRules })
    const ruleMatch = /^\/v2\/permissions\/rules\/([^/]+)$/.exec(pathname)
    if (method === "DELETE" && ruleMatch) {
      const id = decodeURIComponent(ruleMatch[1]!)
      this.savedRules = this.savedRules.filter((r) => r.id !== id)
      return jsonResponse({})
    }

    // Agent models & efforts
    if (method === "GET" && pathname === "/v2/agent-models") return jsonResponse({ agents: this.agentModels })
    const agentConfigMatch = /^\/v2\/agent-models\/([^/]+)\/configuration$/.exec(pathname)
    if (method === "PUT" && agentConfigMatch) {
      const agentId = decodeURIComponent(agentConfigMatch[1]!)
      const row = this.agentModels.find((a) => a.agentId === agentId)
      if (!row) return errorResponse(404, "not_found", `agent ${agentId} not found`)
      const model = body?.model as { providerId?: string; modelId?: string } | undefined
      row.configuration = model
      row.configured = Boolean(model)
      row.effective = model ?? { providerId: "fake", modelId: "model" }
      row.source = model ? "AGENT_MODEL_SOURCE_CONFIGURED" : "AGENT_MODEL_SOURCE_DEFAULT"
      return jsonResponse(row)
    }
    const agentModelMatch = /^\/v2\/agent-models\/([^/]+)$/.exec(pathname)
    if (method === "PUT" && agentModelMatch) {
      const agentId = decodeURIComponent(agentModelMatch[1]!)
      const row = this.agentModels.find((a) => a.agentId === agentId)
      if (!row) return errorResponse(404, "not_found", `agent ${agentId} not found`)
      const pref = body?.preference as { providerId?: string; modelId?: string } | undefined
      row.preference = pref
      row.preferenceAvailable = Boolean(pref)
      row.effective = pref ?? { providerId: "fake", modelId: "model" }
      row.source = pref ? "AGENT_MODEL_SOURCE_REMEMBERED" : "AGENT_MODEL_SOURCE_DEFAULT"
      return jsonResponse(row)
    }
    if (method === "GET" && pathname === "/v2/model-effort-preferences") return jsonResponse({ preferences: this.modelEffortPrefs })
    const modelEffortMatch = /^\/v2\/model-effort-preferences\/([^/]+)\/([^/]+)$/.exec(pathname)
    if (method === "PUT" && modelEffortMatch) {
      const providerId = decodeURIComponent(modelEffortMatch[1]!)
      const modelId = decodeURIComponent(modelEffortMatch[2]!)
      const effort = String(body?.effort ?? "")
      const existing = this.modelEffortPrefs.find((p) => p.providerId === providerId && p.modelId === modelId)
      if (existing) existing.effort = effort || undefined
      else if (effort) this.modelEffortPrefs.push({ providerId, modelId, effort })
      for (const s of this.sessions) {
        if (s.model?.providerId === providerId && s.model?.modelId === modelId && !s.model?.variant) {
          s.effectiveEffort = effort || "default"
          s.effortSource = effort ? "EFFORT_SOURCE_MODEL_PREFERENCE" : "EFFORT_SOURCE_DEFAULT"
        }
      }
      return jsonResponse({ providerId, modelId, ...(effort ? { effort } : {}) })
    }
    const agentEffortMatch = /^\/v2\/agent-efforts\/([^/]+)$/.exec(pathname)
    if (method === "PUT" && agentEffortMatch) {
      const agentId = decodeURIComponent(agentEffortMatch[1]!)
      const effort = String(body?.effort ?? "")
      const row = this.agentModels.find((a) => a.agentId === agentId)
      if (row) {
        row.effort = effort || undefined
        row.effortSource = effort ? "AGENT_EFFORT_SOURCE_PREFERENCE" : "AGENT_EFFORT_SOURCE_NONE"
      }
      for (const s of this.sessions) {
        if (s.agent === agentId) {
          s.effectiveEffort = effort || "default"
          s.effortSource = effort ? "EFFORT_SOURCE_AGENT" : "EFFORT_SOURCE_DEFAULT"
        }
      }
      return jsonResponse({ agentId, ...(effort ? { effort } : {}) })
    }

    // Files
    if (method === "GET" && pathname === "/v2/fs/find") {
      const pattern = url.searchParams.get("pattern") ?? "*"
      const limit = Number(url.searchParams.get("limit") ?? "50")
      const paths = [...this.files.keys()].filter((p) => globMatch(p, pattern)).slice(0, limit)
      return jsonResponse({ paths })
    }
    if (method === "GET" && pathname === "/v2/fs/read") {
      const path = url.searchParams.get("path") ?? ""
      const bytes = this.files.get(path)
      if (!bytes) return errorResponse(404, "not_found", `file not found: ${path}`)
      const mime = path.endsWith(".png") ? "image/png" : path.endsWith(".bmp") ? "image/bmp" : "text/plain"
      return jsonResponse({ content: Buffer.from(bytes).toString("base64"), text: mime === "text/plain", mime })
    }

    return errorResponse(404, "not_found", `unhandled fake route: ${method} ${pathname}`)
  }
}

export interface FakeTerminal extends TerminalAccess {
  copied: string[]
  notifications: string[]
  suspended: number
  resumed: number
  setFocused(focused: boolean): void
}

function createFakeTerminal(copyAccepts = true): FakeTerminal {
  const focusHandlers = new Set<(focused: boolean) => void>()
  const terminal: FakeTerminal = {
    copied: [],
    notifications: [],
    suspended: 0,
    resumed: 0,
    copy(text: string) {
      if (!copyAccepts) return false
      terminal.copied.push(text)
      return true
    },
    suspend() { terminal.suspended++ },
    resume() { terminal.resumed++ },
    notify(sequence: string) { terminal.notifications.push(sequence) },
    onFocusChange(handler) {
      focusHandlers.add(handler)
      return () => focusHandlers.delete(handler)
    },
    setFocused(focused: boolean) {
      for (const handler of focusHandlers) handler(focused)
    },
  }
  return terminal
}

export interface RenderAppOptions {
  width?: number
  height?: number
  seed?: FakeBackendSeed
  /** TUI preset written to isolated `tui.toml` (e.g. `"workspace"`, `"statusline"`, `"minimal"`, `"compact"`). */
  preset?: string
  /** Raw `tui.toml` text (overrides `preset`). */
  tuiToml?: string
  /** Initial `tui.json` preferences. */
  preferences?: TuiPreferences
  /** Persist preferences to a caller-managed directory instead of a fresh temp dir (tests restart persistence). */
  configDir?: string
  remote?: boolean
  serverLabel?: string
  web?: WebInfo
  webTab?: boolean
  startup?: ControllerOptions["startup"]
  env?: Record<string, string | undefined>
  bridge?: ControllerOptions["bridge"]
  home?: ControllerOptions["home"]
  copyAccepts?: boolean
  /** Skip `controller.start()` when testing pre-connect states. */
  skipStart?: boolean
  /** Wire `client.listTuiExtensions()` -> `extensionManager.load(...)` on startup and scope changes (like `app/run.tsx`), resetting `extensionManager` on `dispose()`. */
  loadExtensions?: boolean
}

function rgbaHex(color: RGBA | undefined): string | undefined {
  if (!color) return undefined
  const [r, g, b] = [color.r, color.g, color.b].map((n) => Math.round(n * 255).toString(16).padStart(2, "0"))
  return `#${r}${g}${b}`
}

export interface RenderedApp {
  setup: Awaited<ReturnType<typeof testRender>>
  store: AppStore
  controller: Controller
  client: HyaClient
  backend: FakeBackend
  terminal: FakeTerminal
  structure: StructureControl
  configDir: string
  preferencesPath: string
  tuiConfigPath: string
  quitCalls: Array<"archive" | "background">
  render(): Promise<void>
  frame(): string
  lines(): string[]
  find(pattern: string | RegExp): { row: number; col: number; line: string } | null
  span(text: string): { text: string; fg?: string; bg?: string; attributes: number } | undefined
  waitFor(predicate: () => boolean | Promise<boolean>, what?: string, timeoutMs?: number): Promise<void>
  waitForText(pattern: string | RegExp, timeoutMs?: number): Promise<string>
  waitForGone(pattern: string | RegExp, timeoutMs?: number): Promise<void>
  type(text: string): Promise<void>
  press(key: KeyInput, mods?: KeyModifiers): Promise<void>
  paste(text: string): Promise<void>
  clickText(pattern: string | RegExp, button?: MouseButton): Promise<void>
  resize(width: number, height: number): Promise<void>
  submit(text: string): Promise<void>
  dispose(): Promise<void>
}

export async function renderApp(options: RenderAppOptions = {}): Promise<RenderedApp> {
  const width = options.width ?? 156
  const height = options.height ?? 41
  const backend = new FakeBackend(options.seed)
  const ownConfigDir = !options.configDir
  const configDir = options.configDir ?? mkdtempSync(join(tmpdir(), "hya-tui-harness-"))
  const preferencesPath = join(configDir, "tui.json")
  const tomlPath = join(configDir, "tui.toml")
  if (options.preferences) writeFileSync(preferencesPath, JSON.stringify(options.preferences, null, 2))
  if (options.tuiToml !== undefined) writeFileSync(tomlPath, options.tuiToml)
  else if (options.preset) writeFileSync(tomlPath, `preset = ${JSON.stringify(options.preset)}\n`)

  const loadedPrefs = loadPreferences(preferencesPath)
  setCustomKeybindings(loadedPrefs.preferences.keybindings ?? {})
  setTerminalThemeMode("dark")
  setThemeSetting(loadedPrefs.preferences.theme ?? autoThemeSetting)

  const store = createAppStore()
  const client = new HyaClient("http://127.0.0.1:9999", options.remote ? "" : backend.directory, backend.fetcher)
  store.setServerUrl(client.baseUrl)
  if (options.serverLabel) store.setServerLabel(options.serverLabel)
  if (options.web) store.setWeb(options.web)
  if (options.webTab) store.setWebTab(true)
  if (loadedPrefs.preferences.vim) store.setVim(true)

  const structure = createStructureController({ store, path: tomlPath, preferencesPath })
  const terminal = createFakeTerminal(options.copyAccepts ?? true)
  const quitCalls: Array<"archive" | "background"> = []

  const refreshExtensions = options.loadExtensions
    ? async (): Promise<void> => {
      try {
        const catalog = await client.listTuiExtensions()
        const { extensionEnabled = {}, extensionTrusted = {} } = loadPreferences(preferencesPath).preferences
        const rejected = await extensionManager.load(catalog, {
          enabled: (id) => extensionEnabled[id],
          remote: options.remote === true,
          trusted: (id) => extensionTrusted[id],
        })
        if (rejected.length) store.setStatus(`TUI extensions rejected: ${rejected.join(" · ")}`)
      } catch (error) {
        const reason = error instanceof Error ? error.message : String(error)
        extensionManager.catalogFailed(reason)
        store.setStatus(`TUI extensions unavailable: ${reason}`)
      }
    }
    : undefined

  const controller = createController({
    client,
    store,
    directory: backend.directory,
    remote: options.remote === true,
    quit: (mode) => quitCalls.push(mode),
    startup: options.startup ?? { continue: false },
    preferencesPath,
    preferredPermissionMode: loadedPrefs.preferences.permissionMode,
    terminal,
    env: options.env ?? process.env,
    ...(options.bridge ? { bridge: options.bridge } : {}),
    ...(options.home ? { home: options.home } : {}),
    ...(refreshExtensions ? { refreshExtensions, onScopeChanged: refreshExtensions } : {}),
    structure,
  })

  const setup = await testRender(() => (
    <AppContext.Provider value={{ store, controller, server: client.baseUrl, ui: controller.ui }}>
      <App />
    </AppContext.Provider>
  ), { width, height, exitOnCtrlC: false, autoFocus: false })
  setup.renderer.setMaxListeners(64)

  if (!options.skipStart) {
    await controller.start()
    if (refreshExtensions) await refreshExtensions()
  }
  await setup.renderOnce()

  const frame = (): string => setup.captureCharFrame()
  const lines = (): string[] => frame().split("\n").map((line) => line.trimEnd())
  const matches = (line: string, pattern: string | RegExp): number => {
    if (typeof pattern === "string") return line.indexOf(pattern)
    const found = pattern.exec(line)
    return found ? found.index : -1
  }
  const find = (pattern: string | RegExp) => {
    const all = lines()
    for (let row = 0; row < all.length; row++) {
      const col = matches(all[row]!, pattern)
      if (col >= 0) return { row, col, line: all[row]! }
    }
    return null
  }
  const waitFor = async (predicate: () => boolean | Promise<boolean>, what = "condition", timeoutMs = 3_000): Promise<void> => {
    const deadline = Date.now() + timeoutMs
    for (;;) {
      await setup.renderOnce()
      if (await predicate()) return
      if (Date.now() > deadline) throw new Error(`Timed out waiting for ${what}\n--- frame ---\n${frame()}`)
      await new Promise((resolve) => setTimeout(resolve, 5))
    }
  }
  const waitForText = async (pattern: string | RegExp, timeoutMs = 3_000): Promise<string> => {
    await waitFor(() => typeof pattern === "string" ? frame().includes(pattern) : pattern.test(frame()), String(pattern), timeoutMs)
    return frame()
  }
  const waitForGone = async (pattern: string | RegExp, timeoutMs = 3_000): Promise<void> => {
    await waitFor(() => typeof pattern === "string" ? !frame().includes(pattern) : !pattern.test(frame()), `gone:${String(pattern)}`, timeoutMs)
  }

  return {
    setup, store, controller, client, backend, terminal, structure, configDir, preferencesPath, tuiConfigPath: tomlPath, quitCalls,
    render: async () => { await setup.renderOnce() },
    frame,
    lines,
    find,
    span(text: string) {
      for (const line of setup.captureSpans().lines) {
        for (const item of line.spans) {
          if (item.text.includes(text)) {
            return { text: item.text, fg: rgbaHex(item.fg), bg: rgbaHex(item.bg), attributes: item.attributes }
          }
        }
      }
      return undefined
    },
    waitFor,
    waitForText,
    waitForGone,
    async type(text: string) {
      await setup.mockInput.typeText(text)
      await setup.renderOnce()
    },
    async press(key: KeyInput, mods?: KeyModifiers) {
      setup.mockInput.pressKey(key, mods)
      if ((key === "ESCAPE" || key === "\u001b") && !mods?.ctrl && !mods?.meta) {
        // OpenTUI's stdin parser buffers a lone ESC briefly to distinguish it from Alt+key sequences.
        await new Promise((resolve) => setTimeout(resolve, 25))
      }
      await setup.renderOnce()
    },
    async paste(text: string) {
      await setup.mockInput.pasteBracketedText(text)
      await setup.renderOnce()
    },
    async clickText(pattern: string | RegExp, button?: MouseButton) {
      const hit = find(pattern)
      if (!hit) throw new Error(`clickText could not find ${String(pattern)}\n${frame()}`)
      await setup.mockMouse.click(hit.col + 1, hit.row, button)
      await setup.renderOnce()
    },
    async resize(nextWidth: number, nextHeight: number) {
      setup.resize(nextWidth, nextHeight)
      await setup.renderOnce()
    },
    async submit(text: string) {
      await setup.mockInput.typeText(text)
      setup.mockInput.pressEnter()
      await setup.renderOnce()
    },
    async dispose() {
      backend.closeAllStreams()
      controller.dispose()
      if (!setup.renderer.isDestroyed) setup.renderer.destroy()
      if (options.loadExtensions) await extensionManager.reset()
      setThemeSetting(autoThemeSetting)
      setTerminalThemeMode("dark")
      setCustomKeybindings({})
      if (ownConfigDir) rmSync(configDir, { recursive: true, force: true })
    },
  }
}

let basicComponentsDir: string | undefined

/** Load the real first-party `hya/basic-tui-components` extension once for suites testing Sessions/Projects/Todos/Context panes. */
export async function loadBasicComponents(): Promise<void> {
  if (extensionManager.list().some((e) => e.id === "hya/basic-tui-components" && e.state === "running")) return
  const bundleDir = join(import.meta.dir, "../../../../bundles/builtin/basic-tui-components")
  const yaml = readFileSync(join(bundleDir, "bundle.yaml"), "utf8")
  const files = [...yaml.matchAll(/path: (tui\/[^ }]+)/g)].map((m) => m[1]!)
  const permissions = [...yaml.slice(yaml.indexOf("\ntui:")).matchAll(/^    - (.+)$/gm)].map((m) => m[1]!)
  const sha = (text: string) => createHash("sha256").update(text).digest("hex")
  const fileEntries = files.map((path) => {
    const content = readFileSync(join(bundleDir, path), "utf8")
    return { path, sha256: sha(content), content }
  })
  const digest = sha(fileEntries.map((f) => `${f.path}:${f.sha256}`).join("\n"))
  basicComponentsDir ??= mkdtempSync(join(tmpdir(), "hya-tui-basic-ext-"))
  extensionManager.configure({
    cacheRoot: join(basicComponentsDir, "cache"),
    sdk: locateSdk(),
    sandbox: "disabled",
    vmDeadlineMs: 500,
    restartDelayMs: 25,
  })
  await extensionManager.load([{
    bundleId: "hya/basic-tui-components",
    bundleVersion: "1.0.0",
    preparedDigest: digest,
    apiVersion: 2,
    entry: "tui/main.ts",
    sdk: "2.0.0",
    permissions,
    firstParty: true,
    files: fileEntries,
  }], { enabled: () => undefined, remote: false, trusted: () => true })
  await extensionManager.settled()
}

/** Restore the global `extensionManager` singleton to pre-catalog state in `afterAll()`. */
export async function resetBasicComponents(): Promise<void> {
  await extensionManager.reset()
  if (basicComponentsDir) {
    rmSync(basicComponentsDir, { recursive: true, force: true })
    basicComponentsDir = undefined
  }
}
