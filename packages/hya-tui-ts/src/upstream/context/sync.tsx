import type {
  Message,
  Agent,
  Provider,
  Session,
  Part,
  Config,
  Todo,
  Command,
  PermissionRequest,
  QuestionRequest,
  LspStatus,
  McpStatus,
  McpResource,
  FormatterStatus,
  SessionStatus,
  ProviderListResponse,
  VcsInfo,
  SnapshotFileDiff,
} from "@opencode-ai/sdk/v2"
import { createStore, produce, reconcile } from "solid-js/store"
import { useProject } from "./project"
import { useEvent } from "./event"
import { useSDK } from "./sdk"
import { useTuiStartup } from "./runtime"
import { createSimpleContext } from "./helper"
import { useExit } from "./exit"
import { useArgs } from "./args"
import { batch, onMount } from "solid-js"
import path from "path"
import { startupMark } from "../../hya/startup-trace"
import { useKV } from "./kv"
import { decodeAgentModels, supportsAgentModelPreferences, type AgentModelIdentity, type AgentModelState } from "../../hya/agent-models"
import { decodeCatalogSelection, type CatalogSelection } from "../../hya/model-catalog"

/**
 * Compare Session cache keys using JavaScript code-unit ordering.
 * @param left The first Session cache key.
 * @param right The second Session cache key.
 * @returns -1 when left precedes right, 0 when equal, or 1 when left follows right.
 */
function compareSessionIDs(left: string, right: string): number {
  if (left === right) return 0
  return left < right ? -1 : 1
}
function search<T>(items: T[], target: string, key: (item: T) => string) {
  let left = 0
  let right = items.length - 1
  while (left <= right) {
    const middle = Math.floor((left + right) / 2)
    const value = key(items[middle])
    const comparison = compareSessionIDs(value, target)
    if (comparison === 0) return { found: true, index: middle }
    if (comparison < 0) left = middle + 1
    else right = middle - 1
  }
  return { found: false, index: left }
}

export const {
  context: SyncContext,
  use: useSync,
  provider: SyncProvider,
} = createSimpleContext({
  name: "Sync",
  init: () => {
    const startup = useTuiStartup()
    const kv = useKV()
    const [store, setStore] = createStore<{
      status: "loading" | "partial" | "complete"
      provider: Provider[]
      provider_default: Record<string, string>
      provider_catalog_default: CatalogSelection | undefined
      provider_next: ProviderListResponse
      capabilities: {
        experimentalBackgroundSubagents: boolean
        agentModelPreferences: boolean
      }
      agent: Agent[]
      agentModels: AgentModelState[]
      command: Command[]
      permission: {
        [sessionID: string]: PermissionRequest[]
      }
      question: {
        [sessionID: string]: QuestionRequest[]
      }
      config: Config
      session: Session[]
      session_status: {
        [sessionID: string]: SessionStatus
      }
      session_diff: {
        [sessionID: string]: SnapshotFileDiff[]
      }
      todo: {
        [sessionID: string]: Todo[]
      }
      message: {
        [sessionID: string]: Message[]
      }
      part: {
        [messageID: string]: Part[]
      }
      lsp: LspStatus[]
      mcp: {
        [key: string]: McpStatus
      }
      mcp_resource: {
        [key: string]: McpResource
      }
      formatter: FormatterStatus[]
      vcs: VcsInfo | undefined
    }>({
      provider_next: {
        all: [],
        default: {},
        connected: [],
      },
      capabilities: {
        experimentalBackgroundSubagents: false,
        agentModelPreferences: false,
      },
      config: {},
      status: "loading",
      agent: [],
      agentModels: [],
      permission: {},
      question: {},
      command: [],
      provider: [],
      provider_default: {},
      provider_catalog_default: undefined,
      session: [],
      session_status: {},
      session_diff: {},
      todo: {},
      message: {},
      part: {},
      lsp: [],
      mcp: {},
      mcp_resource: {},
      formatter: [],
      vcs: undefined,
    })
    const event = useEvent()
    const project = useProject()
    const sdk = useSDK()
    /**
     * Build a dedicated Agent model URL with the current workspace query.
     * @param pathname Dedicated control route path.
     * @param workspace Current synchronized workspace, when known.
     * @returns An absolute URL for the existing SDK transport.
     */
    function agentModelURL(pathname: string, workspace: string | undefined): string {
      const url = new URL(pathname, sdk.url)
      if (workspace) url.searchParams.set("directory", workspace)
      return url.toString()
    }

    /**
     * Read a JSON response and add operation context when decoding fails.
     * @param response SDK transport response.
     * @param operation Human-readable operation name.
     * @returns The unknown decoded JSON payload.
     */
    async function readAgentModelJSON(response: Response, operation: string): Promise<unknown> {
      try {
        return (await response.json()) as unknown
      } catch (error) {
        const reason = error instanceof Error ? error.message : String(error)
        throw new Error(`${operation} returned invalid JSON: ${reason}`)
      }
    }

    /**
     * Convert a failed Agent model response to one bounded contextual error.
     * @param response Failed SDK transport response.
     * @param operation Human-readable operation name.
     * @returns A bounded error for toast and caller handling.
     */
    async function agentModelResponseError(response: Response, operation: string): Promise<Error> {
      let detail = ""
      try {
        const text = (await response.text()).trim().slice(0, 512)
        if (text) {
          try {
            const payload = JSON.parse(text) as unknown
            const record = payload && typeof payload === "object" && !Array.isArray(payload)
              ? (payload as Record<string, unknown>)
              : undefined
            const error = record?.error && typeof record.error === "object" && !Array.isArray(record.error)
              ? (record.error as Record<string, unknown>)
              : undefined
            const message = error?.message ?? record?.message
            detail = typeof message === "string" ? message.slice(0, 512) : text
          } catch {
            detail = text
          }
        }
      } catch (error) {
        detail = `unable to read error response: ${error instanceof Error ? error.message : String(error)}`
      }
      return new Error(`${operation} failed (${response.status})${detail ? `: ${detail}` : ""}`)
    }

    /**
     * Fetch and decode the complete Agent model preference list.
     * @param workspace Current synchronized workspace, when known.
     * @param providers Provider catalog paired with the response.
     * @returns Allowlisted normalized Agent model rows.
     */
    async function fetchAgentModelRows(
      workspace: string | undefined,
      providers: unknown = store.provider,
    ): Promise<AgentModelState[]> {
      const response = await sdk.fetch(agentModelURL("/tui/agent-models", workspace), {
        headers: {
          "x-opencode-directory": sdk.directory ?? "",
          accept: "application/json",
        },
      })
      if (!response.ok) throw await agentModelResponseError(response, "Agent model refresh")
      return decodeAgentModels(await readAgentModelJSON(response, "Agent model refresh"), providers)
    }

    /**
     * Refresh synchronized Agent model rows when the backend advertises support.
     * @returns The refreshed rows, or current rows when unsupported.
     */
    async function refreshAgentModels(): Promise<AgentModelState[]> {
      if (!store.capabilities.agentModelPreferences) return store.agentModels
      const rows = await fetchAgentModelRows(project.workspace.current())
      setStore("agentModels", reconcile(rows))
      return rows
    }

    /**
     * Persist one Agent model preference and replace only its synchronized row.
     * @param agentID Exact stable Agent id.
     * @param preference New base-model identity, or null to clear it.
     * @returns The normalized post-commit row.
     */
    async function setAgentModelPreference(
      agentID: string,
      preference: AgentModelIdentity | null,
    ): Promise<AgentModelState> {
      if (!store.capabilities.agentModelPreferences) {
        throw new Error("Agent model preferences are unavailable on this backend")
      }
      const response = await sdk.fetch(
        agentModelURL(`/tui/agent-models/${encodeURIComponent(agentID)}`, project.workspace.current()),
        {
          method: "PUT",
          headers: {
            "x-opencode-directory": sdk.directory ?? "",
            accept: "application/json",
            "content-type": "application/json",
          },
          body: JSON.stringify({
            preference:
              preference === null
                ? null
                : { providerID: preference.providerID, modelID: preference.modelID },
          }),
        },
      )
      if (!response.ok) throw await agentModelResponseError(response, "Agent model update")
      const payload = await readAgentModelJSON(response, "Agent model update")
      const decoded = decodeAgentModels([payload], store.provider)
      const row = decoded.length === 1 ? decoded[0] : undefined
      if (!row || row.agentID !== agentID) {
        throw new Error(`Agent model update returned no normalized row for ${agentID}`)
      }
      const index = store.agentModels.findIndex((item) => item.agentID === agentID)
      if (index < 0) throw new Error(`Agent model update returned an unknown row for ${agentID}`)
      setStore("agentModels", index, reconcile(row))
      return row
    }


    const fullSyncedSessions = new Set<string>()
    const syncingSessions = new Map<string, Promise<void>>()
    const hydratingSessions = new Map<string, { messages: Set<string>; parts: Set<string> }>()
    const touchMessage = (sessionID: string, messageID: string) => {
      hydratingSessions.get(sessionID)?.messages.add(messageID)
    }
    const touchPart = (sessionID: string, partID: string) => {
      hydratingSessions.get(sessionID)?.parts.add(partID)
    }

    function sessionListQuery(): { scope?: "project"; path?: string } {
      if (!kv.get("session_directory_filter_enabled", true)) return { scope: "project" }
      if (!project.data.instance.path.worktree || !project.data.instance.path.directory) return { scope: "project" }
      return {
        path: path
          .relative(path.resolve(project.data.instance.path.worktree), project.data.instance.path.directory)
          .replaceAll("\\", "/"),
      }
    }

    /**
     * Normalize Session rows for binary-search cache lookups.
     * @param sessions Session rows in any order.
     * @returns A new Session array sorted ascending by id.
     */
    function sortSessionsByID(sessions: Session[]): Session[] {
      return sessions.toSorted((a, b) => compareSessionIDs(a.id, b.id))
    }
    function listSessions() {
      return sdk.client.session
        .list({ start: Date.now() - 30 * 24 * 60 * 60 * 1000, ...sessionListQuery() })
        .then((x) => sortSessionsByID(x.data ?? []))
    }

    event.subscribe((event, { workspace }) => {
      switch (event.type) {
        case "server.instance.disposed":
          void bootstrap()
          break
        case "permission.replied": {
          const requests = store.permission[event.properties.sessionID]
          if (!requests) break
          const match = search(requests, event.properties.requestID, (r) => r.id)
          if (!match.found) break
          setStore(
            "permission",
            event.properties.sessionID,
            produce((draft) => {
              draft.splice(match.index, 1)
            }),
          )
          break
        }

        case "permission.asked": {
          const request = event.properties
          const requests = store.permission[request.sessionID]
          if (!requests) {
            setStore("permission", request.sessionID, [request])
            break
          }
          const match = search(requests, request.id, (r) => r.id)
          if (match.found) {
            setStore("permission", request.sessionID, match.index, reconcile(request))
            break
          }
          setStore(
            "permission",
            request.sessionID,
            produce((draft) => {
              draft.splice(match.index, 0, request)
            }),
          )
          break
        }

        case "question.replied":
        case "question.rejected": {
          const requests = store.question[event.properties.sessionID]
          if (!requests) break
          const match = search(requests, event.properties.requestID, (r) => r.id)
          if (!match.found) break
          setStore(
            "question",
            event.properties.sessionID,
            produce((draft) => {
              draft.splice(match.index, 1)
            }),
          )
          break
        }

        case "question.asked": {
          const request = event.properties
          const requests = store.question[request.sessionID]
          if (!requests) {
            setStore("question", request.sessionID, [request])
            break
          }
          const match = search(requests, request.id, (r) => r.id)
          if (match.found) {
            setStore("question", request.sessionID, match.index, reconcile(request))
            break
          }
          setStore(
            "question",
            request.sessionID,
            produce((draft) => {
              draft.splice(match.index, 0, request)
            }),
          )
          break
        }

        case "todo.updated":
          setStore("todo", event.properties.sessionID, event.properties.todos)
          break

        case "session.diff":
          setStore("session_diff", event.properties.sessionID, event.properties.diff)
          break

        case "session.deleted": {
          const result = search(store.session, event.properties.info.id, (s) => s.id)
          if (result.found) {
            setStore(
              "session",
              produce((draft) => {
                draft.splice(result.index, 1)
              }),
            )
          }
          break
        }
        case "session.updated": {
          const result = search(store.session, event.properties.info.id, (s) => s.id)
          if (result.found) {
            setStore("session", result.index, reconcile(event.properties.info))
            break
          }
          setStore(
            "session",
            produce((draft) => {
              draft.splice(result.index, 0, event.properties.info)
            }),
          )
          break
        }

        case "session.next.moved": {
          const result = search(store.session, event.properties.sessionID, (s) => s.id)
          if (!result.found) break
          setStore(
            "session",
            result.index,
            produce((session) => {
              session.directory = event.properties.location.directory
              session.path = event.properties.subdirectory
              session.workspaceID = event.properties.location.workspaceID
              session.time.updated = event.properties.timestamp
            }),
          )
          break
        }

        case "session.status": {
          setStore("session_status", event.properties.sessionID, event.properties.status)
          break
        }

        case "message.updated": {
          touchMessage(event.properties.info.sessionID, event.properties.info.id)
          const messages = store.message[event.properties.info.sessionID]
          if (!messages) {
            setStore("message", event.properties.info.sessionID, [event.properties.info])
            break
          }
          const result = search(messages, event.properties.info.id, (m) => m.id)
          if (result.found) {
            setStore("message", event.properties.info.sessionID, result.index, reconcile(event.properties.info))
            break
          }
          setStore(
            "message",
            event.properties.info.sessionID,
            produce((draft) => {
              draft.splice(result.index, 0, event.properties.info)
            }),
          )
          const updated = store.message[event.properties.info.sessionID]
          if (updated.length > 100) {
            const oldest = updated[0]
            batch(() => {
              setStore(
                "message",
                event.properties.info.sessionID,
                produce((draft) => {
                  draft.shift()
                }),
              )
              setStore(
                "part",
                produce((draft) => {
                  delete draft[oldest.id]
                }),
              )
            })
          }
          break
        }
        case "message.removed": {
          touchMessage(event.properties.sessionID, event.properties.messageID)
          const messages = store.message[event.properties.sessionID]
          const result = search(messages, event.properties.messageID, (m) => m.id)
          if (result.found) {
            setStore(
              "message",
              event.properties.sessionID,
              produce((draft) => {
                draft.splice(result.index, 1)
              }),
            )
          }
          break
        }
        case "message.part.updated": {
          touchPart(event.properties.part.sessionID, event.properties.part.id)
          const parts = store.part[event.properties.part.messageID]
          if (!parts) {
            setStore("part", event.properties.part.messageID, [event.properties.part])
            break
          }
          const result = search(parts, event.properties.part.id, (p) => p.id)
          if (result.found) {
            setStore("part", event.properties.part.messageID, result.index, reconcile(event.properties.part))
            break
          }
          setStore(
            "part",
            event.properties.part.messageID,
            produce((draft) => {
              draft.splice(result.index, 0, event.properties.part)
            }),
          )
          break
        }

        case "message.part.delta": {
          const parts = store.part[event.properties.messageID]
          if (!parts) break
          const result = search(parts, event.properties.partID, (p) => p.id)
          if (!result.found) break
          touchPart(event.properties.sessionID, event.properties.partID)
          setStore(
            "part",
            event.properties.messageID,
            produce((draft) => {
              const part = draft[result.index]
              const field = event.properties.field as keyof typeof part
              const existing = part[field] as string | undefined
              ;(part[field] as string) = (existing ?? "") + event.properties.delta
            }),
          )
          break
        }

        case "message.part.removed": {
          touchPart(event.properties.sessionID, event.properties.partID)
          const parts = store.part[event.properties.messageID]
          const result = search(parts, event.properties.partID, (p) => p.id)
          if (result.found) {
            setStore(
              "part",
              event.properties.messageID,
              produce((draft) => {
                draft.splice(result.index, 1)
              }),
            )
          }
          break
        }

        case "lsp.updated": {
          const workspace = project.workspace.current()
          void sdk.client.lsp.status({ workspace }).then((x) => setStore("lsp", x.data ?? []))
          break
        }

        case "vcs.branch.updated": {
          if (workspace === project.workspace.current()) {
            setStore("vcs", { branch: event.properties.branch })
          }
          break
        }
      }
    })

    const exit = useExit()
    const args = useArgs()

    async function bootstrap(input: { fatal?: boolean } = {}) {
      const fatal = input.fatal ?? true
      const workspace = project.workspace.current()

      try {
        // Prefer single-RTT /tui/bootstrap (hya servers). Fall back to multi-call for older backends.
        if (await bootstrapViaBundle(workspace)) {
          return
        }
        await bootstrapViaMultiCall(workspace)
      } catch (e) {
        console.error("tui bootstrap failed", {
          error: e instanceof Error ? e.message : String(e),
          name: e instanceof Error ? e.name : undefined,
          stack: e instanceof Error ? e.stack : undefined,
        })
        if (fatal) {
          exit(e)
        } else {
          throw e
        }
      }
    }

    type BootstrapBundle = {
      config?: unknown
      providers?: { providers?: unknown; default?: unknown; defaultModel?: unknown }
      provider_list?: unknown
      capabilities?: { backgroundSubagents?: unknown; agentModelPreferences?: unknown }
      agentModels?: unknown[]
      agents?: unknown[]
      sessions?: Session[]
      commands?: unknown[]
      lsp?: unknown[]
      mcp?: Record<string, unknown>
      mcp_resource?: Record<string, unknown>
      formatter?: unknown[]
      session_status?: Record<string, unknown>
      vcs?: unknown
      path?: unknown
      project?: { id?: string; worktree?: string }
    }

    async function bootstrapViaBundle(workspace: string | undefined): Promise<boolean> {
      const url = new URL("/tui/bootstrap", sdk.url)
      if (workspace) url.searchParams.set("directory", workspace)
      const response = await sdk.fetch(url.toString(), {
        headers: {
          "x-opencode-directory": sdk.directory ?? "",
          accept: "application/json",
        },
      })
      if (!response.ok) return false
      const bundle = (await response.json()) as BootstrapBundle
      // Reject non-object payloads (legacy mocks often return []).
      if (!bundle || typeof bundle !== "object" || Array.isArray(bundle)) return false
      if (bundle.providers === undefined && bundle.config === undefined && bundle.agents === undefined) {
        return false
      }
      applyBootstrapBundle(bundle)
      startupMark("sync_partial", "bundle")
      startupMark("sync_complete", "bundle")
      return true
    }

    function applyBootstrapBundle(bundle: BootstrapBundle) {
      const providers = bundle.providers?.providers ?? []
      const providerPayloadKnown = bundle.providers?.providers !== undefined
      const catalogDefault = decodeCatalogSelection(bundle.providers?.defaultModel, providers)
      const agentModelCapability = supportsAgentModelPreferences(bundle.capabilities)
      const agentModels =
        providerPayloadKnown && agentModelCapability
          ? decodeAgentModels(bundle.agentModels ?? [], providers)
          : []
      batch(() => {
        if (bundle.providers) {
          setStore("provider", reconcile(providers as never))
          setStore("provider_default", reconcile((bundle.providers.default as never) ?? {}))
          setStore("provider_catalog_default", catalogDefault)
        }
        if (bundle.provider_list !== undefined) {
          setStore("provider_next", reconcile(bundle.provider_list as never))
        }
        setStore("capabilities", "experimentalBackgroundSubagents", bundle.capabilities?.backgroundSubagents === true)
        setStore("capabilities", "agentModelPreferences", agentModelCapability)
        if (providerPayloadKnown) setStore("agentModels", reconcile(agentModels))
        if (bundle.agents) setStore("agent", reconcile(bundle.agents as never))
        if (bundle.config !== undefined) setStore("config", reconcile(bundle.config as never))
        if (bundle.sessions) setStore("session", reconcile(sortSessionsByID(bundle.sessions)))
        if (bundle.commands) setStore("command", reconcile(bundle.commands as never))
        if (bundle.lsp) setStore("lsp", reconcile(bundle.lsp as never))
        if (bundle.mcp) setStore("mcp", reconcile(bundle.mcp as never))
        if (bundle.mcp_resource) setStore("mcp_resource", reconcile(bundle.mcp_resource as never))
        if (bundle.formatter) setStore("formatter", reconcile(bundle.formatter as never))
        if (bundle.session_status) setStore("session_status", reconcile(bundle.session_status as never))
        if (bundle.vcs !== undefined) setStore("vcs", reconcile(bundle.vcs as never))
        setStore("status", "complete")
      })
      // Path/project sync is still owned by ProjectProvider; kick it without blocking complete.
      void project.sync().catch(() => undefined)
    }

    async function bootstrapViaMultiCall(workspace: string | undefined) {
      const projectPromise = project.sync()
      const sessionListPromise = projectPromise.then(() => listSessions())

      // blocking - include session.list when continuing a session
      const providersPromise = sdk.client.config.providers({ workspace }, { throwOnError: true })
      const providerListPromise = sdk.client.provider.list({ workspace }, { throwOnError: true })
      const capabilitiesPromise = sdk.client.experimental.capabilities
        .get({ workspace }, { throwOnError: true })
        .then((x) => x.data)
        .catch(() => undefined)
      const agentsPromise = sdk.client.app.agents({ workspace }, { throwOnError: true })
      const configPromise = sdk.client.config.get({ workspace }, { throwOnError: true })
      await Promise.all([
        providersPromise,
        providerListPromise,
        capabilitiesPromise,
        agentsPromise,
        configPromise,
        projectPromise,
        ...(args.continue ? [sessionListPromise] : []),
      ])
      const providers = (await providersPromise).data!
      const providerList = (await providerListPromise).data!
      const capabilities = await capabilitiesPromise
      const agents = (await agentsPromise).data ?? []
      const config = (await configPromise).data!
      const sessions = args.continue ? await sessionListPromise : undefined
      const catalogDefault = decodeCatalogSelection(
        (providers as typeof providers & { defaultModel?: unknown }).defaultModel,
        providers.providers,
      )
      const agentModelCapability = supportsAgentModelPreferences(capabilities)
      const agentModels = agentModelCapability ? await fetchAgentModelRows(workspace, providers.providers) : []

      batch(() => {
        setStore("provider", reconcile(providers.providers))
        setStore("provider_default", reconcile(providers.default))
        setStore("provider_catalog_default", catalogDefault)
        setStore("provider_next", reconcile(providerList))
        setStore("capabilities", "experimentalBackgroundSubagents", capabilities?.backgroundSubagents === true)
        setStore("capabilities", "agentModelPreferences", agentModelCapability)
        setStore("agentModels", reconcile(agentModels))
        setStore("agent", reconcile(agents))
        setStore("config", reconcile(config))
        if (sessions !== undefined) setStore("session", reconcile(sessions))
      })
      if (store.status !== "complete") setStore("status", "partial")
      startupMark("sync_partial", "multi")

      await Promise.all([
        ...(args.continue ? [] : [sessionListPromise.then((list) => setStore("session", reconcile(list)))]),
        sdk.client.command.list({ workspace }).then((x) => setStore("command", reconcile(x.data ?? []))),
        sdk.client.lsp.status({ workspace }).then((x) => setStore("lsp", reconcile(x.data ?? []))),
        sdk.client.mcp.status({ workspace }).then((x) => setStore("mcp", reconcile(x.data ?? {}))),
        sdk.client.experimental.resource
          .list({ workspace })
          .then((x) => setStore("mcp_resource", reconcile(x.data ?? {}))),
        sdk.client.formatter.status({ workspace }).then((x) => setStore("formatter", reconcile(x.data ?? []))),
        sdk.client.session.status({ workspace }).then((x) => {
          setStore("session_status", reconcile(x.data ?? {}))
        }),
        sdk.client.vcs.get({ workspace }).then((x) => setStore("vcs", reconcile(x.data))),
      ])
      setStore("status", "complete")
      startupMark("sync_complete", "multi")
    }

    onMount(() => {
      void bootstrap()
    })

    const result = {
      data: store,
      set: setStore,
      get status() {
        return store.status
      },
      get ready() {
        if (startup.skipInitialLoading) return true
        return store.status !== "loading"
      },
      get path() {
        return project.instance.path()
      },
      /**
       * Return one synchronized Agent model row by stable Agent id.
       * @param agentID Exact stable Agent id.
       * @returns The normalized row, or undefined when absent.
       */
      getAgentModel(agentID: string) {
        return store.agentModels.find((item) => item.agentID === agentID)
      },
      /** Refresh synchronized Agent model rows through the dedicated backend route. */
      refreshAgentModels,
      /** Persist one Agent model preference and update its synchronized row on success. */
      setAgentModelPreference,
      session: {
        get(sessionID: string) {
          const match = search(store.session, sessionID, (s) => s.id)
          if (match.found) return store.session[match.index]
          return undefined
        },
        query() {
          return sessionListQuery()
        },
        async refresh() {
          const list = await listSessions()
          setStore("session", reconcile(list))
        },
        status(sessionID: string) {
          const session = result.session.get(sessionID)
          if (!session) return "idle"
          if (session.time.compacting) return "compacting"
          const messages = store.message[sessionID] ?? []
          const last = messages.at(-1)
          if (!last) return "idle"
          if (last.role === "user") return "working"
          return last.time.completed ? "idle" : "working"
        },
        async sync(sessionID: string) {
          if (fullSyncedSessions.has(sessionID)) return
          const syncing = syncingSessions.get(sessionID)
          if (syncing) return syncing
          const tracker = { messages: new Set<string>(), parts: new Set<string>() }
          hydratingSessions.set(sessionID, tracker)
          const task = (async () => {
            const [session, messages, todo, diff] = await Promise.all([
              sdk.client.session.get({ sessionID }, { throwOnError: true }),
              sdk.client.session.messages({ sessionID, limit: 100 }),
              sdk.client.session.todo({ sessionID }),
              sdk.client.session.diff({ sessionID }),
            ])
            if (!session.data) throw new Error(`Session not found: ${sessionID}`)

            const currentMessages = store.message[sessionID] ?? []
            const fetchedMessages = messages.data ?? []
            const infos = fetchedMessages.flatMap((message) => {
              if (!tracker.messages.has(message.info.id)) return [message.info]
              const current = currentMessages.find((item) => item.id === message.info.id)
              return current ? [current] : []
            })
            infos.push(
              ...currentMessages.filter(
                (message) => tracker.messages.has(message.id) && !infos.some((item) => item.id === message.id),
              ),
            )
            const removed = infos.slice(0, -100)
            const visible = infos.slice(-100)
            const visibleIDs = new Set(visible.map((message) => message.id))
            const removedPartIDs = removed.map((message) => message.id)
            const hydratedParts: Array<{ messageID: string; parts: Part[] }> = []

            for (const message of fetchedMessages) {
              if (!visibleIDs.has(message.info.id)) {
                removedPartIDs.push(message.info.id)
                continue
              }
              const currentParts = store.part[message.info.id] ?? []
              const parts = message.parts.flatMap((part) => {
                const current = currentParts.find((item) => item.id === part.id)
                if (tracker.parts.has(part.id)) return current ? [current] : []
                if (
                  current &&
                  (part.type === "text" || part.type === "reasoning") &&
                  (current.type === "text" || current.type === "reasoning") &&
                  part.text.length === 0 &&
                  current.text.length > 0
                ) {
                  return [current]
                }
                return [part]
              })
              parts.push(
                ...currentParts.filter(
                  (part) => tracker.parts.has(part.id) && !parts.some((item) => item.id === part.id),
                ),
              )
              hydratedParts.push({ messageID: message.info.id, parts })
            }

            batch(() => {
              const match = search(store.session, sessionID, (s) => s.id)
              if (match.found) setStore("session", match.index, reconcile(session.data))
              else {
                setStore(
                  "session",
                  produce((draft) => {
                    draft.splice(match.index, 0, session.data)
                  }),
                )
              }
              setStore("todo", sessionID, reconcile(todo.data ?? []))
              if (removedPartIDs.length > 0) {
                setStore(
                  "part",
                  produce((draft) => {
                    for (const messageID of removedPartIDs) delete draft[messageID]
                  }),
                )
              }
              for (const hydrated of hydratedParts) {
                setStore("part", hydrated.messageID, reconcile(hydrated.parts))
              }
              setStore("message", sessionID, reconcile(visible))
              setStore("session_diff", sessionID, reconcile(diff.data ?? []))
            })
            fullSyncedSessions.add(sessionID)
          })().finally(() => {
            syncingSessions.delete(sessionID)
            hydratingSessions.delete(sessionID)
          })
          syncingSessions.set(sessionID, task)
          return task
        },
      },
      bootstrap,
    }
    return result
  },
})
