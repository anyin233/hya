/**
 * The TUI's single state store.
 *
 * It holds a copy of the server projection read over the v1 API (sessions,
 * transcript, interactions, catalogs) plus local UI state (view, status line,
 * concealed key entry). Every field is a Solid signal, so components re-render
 * when a mutation runs; mutations are the only way to change state.
 *
 * The store never holds a secret: key entry keeps only the provider id and the
 * bullet mask here; the key itself stays in `SecretEntry` (see completion.ts).
 */
import { batch, createSignal, type Accessor, type Setter } from "solid-js"
import { apiOperationNames, operations } from "../api"
import type {
  AgentSummary,
  Bootstrap,
  CommandSummary,
  Interaction,
  MessageInfo,
  ModelSummary,
  ProviderSummary,
  SessionInfo,
  WorkflowSummary,
} from "../client"
import type { CompletionContext } from "../completion"
import type { View } from "../instructions"

export interface AppState {
  /** False until the first catalog refresh (or connection failure) lands. */
  readonly ready: boolean
  readonly sessions: SessionInfo[]
  readonly messages: MessageInfo[]
  readonly interactions: Interaction[]
  readonly agents: AgentSummary[]
  readonly models: ModelSummary[]
  readonly providers: ProviderSummary[]
  readonly savedKeys: string[]
  /** False when the backend has no key-listing route. */
  readonly savedKeysAvailable: boolean
  readonly backendCommands: CommandSummary[]
  readonly workflows: WorkflowSummary[]
  readonly workflowState: Record<string, unknown> | undefined
  readonly selected: SessionInfo | undefined
  /** Message id of the turn this client admitted, or "". */
  readonly turnId: string
  /** Last event sequence seen on the session stream. */
  readonly cursor: string
  readonly view: View
  readonly apiOutput: string
  readonly status: string
  /** Provider whose key is being entered; undefined outside key entry. */
  readonly secretProvider: string | undefined
  readonly secretMask: string
}

/** Rows loaded by one full catalog refresh. `savedKeys: null` = listing unsupported. */
export interface Catalog {
  sessions: SessionInfo[]
  interactions: Interaction[]
  models: ModelSummary[]
  workflows: WorkflowSummary[]
  providers: ProviderSummary[]
  savedKeys: string[] | null
  commands: CommandSummary[]
}

export const startupStatus = "Enter prompt · /help commands · Ctrl+R refresh · Ctrl+C quit"

function initialState(): { [K in keyof AppState]: AppState[K] } {
  return {
    ready: false,
    sessions: [],
    messages: [],
    interactions: [],
    agents: [],
    models: [],
    providers: [],
    savedKeys: [],
    savedKeysAvailable: true,
    backendCommands: [],
    workflows: [],
    workflowState: undefined,
    selected: undefined,
    turnId: "",
    cursor: "0",
    view: "chat",
    apiOutput: "Use /api METHOD /v1/path [JSON object] to call any HTTP/JSON endpoint.\n\n" + operations(),
    status: startupStatus,
    secretProvider: undefined,
    secretMask: "",
  }
}

type Signals = { [K in keyof AppState]: [Accessor<AppState[K]>, Setter<AppState[K]>] }

export function createAppStore() {
  const initial = initialState()
  const signals = Object.fromEntries(
    Object.entries(initial).map(([key, value]) => [key, createSignal(value, { equals: false })]),
  ) as unknown as Signals
  const state = Object.defineProperties({} as AppState, Object.fromEntries(
    Object.keys(initial).map((key) => [key, { enumerable: true, get: () => signals[key as keyof AppState][0]() }]),
  ))
  const set = <K extends keyof AppState>(key: K, value: AppState[K]): void => {
    (signals[key][1] as (value: AppState[K]) => void)(value)
  }

  return {
    state,

    applyBootstrap(bootstrap: Bootstrap): void {
      batch(() => {
        set("agents", bootstrap.agents ?? [])
        set("models", bootstrap.models ?? [])
        set("interactions", bootstrap.interactions ?? [])
      })
    },

    applyCatalog(catalog: Catalog): void {
      batch(() => {
        set("sessions", catalog.sessions)
        set("interactions", catalog.interactions)
        set("models", catalog.models)
        set("workflows", catalog.workflows)
        set("providers", catalog.providers)
        set("savedKeysAvailable", catalog.savedKeys !== null)
        set("savedKeys", catalog.savedKeys ?? [])
        set("backendCommands", catalog.commands)
        const selected = state.selected
        if (selected) set("selected", catalog.sessions.find((row) => row.id === selected.id) ?? selected)
        set("ready", true)
      })
    },

    /** Mark the UI as painted without data (connection failure). */
    markReady(): void { set("ready", true) },

    /** Select a session: chat view, empty transcript, stream resumes from its last sequence. */
    openSession(session: SessionInfo): void {
      batch(() => {
        set("selected", session)
        set("view", "chat")
        set("cursor", session.lastSeq ?? "0")
        set("messages", [])
      })
    },

    setSelected(session: SessionInfo): void { set("selected", session) },

    /** Store a transcript page; ignored (returns false) when another session is selected. */
    setMessages(sessionId: string, rows: MessageInfo[]): boolean {
      if (state.selected?.id !== sessionId) return false
      set("messages", rows)
      return true
    },

    setInteractions(rows: Interaction[]): void { set("interactions", rows) },
    setWorkflowState(value: Record<string, unknown> | undefined): void { set("workflowState", value) },
    setView(view: View): void { set("view", view) },
    setStatus(text: string): void { set("status", text) },
    setApiOutput(text: string): void { set("apiOutput", text) },
    setCursor(seq: string): void { set("cursor", seq) },

    /** Move the stream cursor forward; older sequences are ignored. */
    advanceCursor(seq: string): void {
      if (BigInt(seq) > BigInt(state.cursor)) set("cursor", seq)
    },

    setTurn(id: string): void { set("turnId", id) },

    /** Clear the active turn when `messageId` is it; returns whether it was. */
    finishTurn(messageId: string): boolean {
      if (!state.turnId || messageId !== state.turnId) return false
      set("turnId", "")
      return true
    },

    beginSecret(provider: string): void {
      batch(() => {
        set("secretProvider", provider)
        set("secretMask", "")
      })
    },
    setSecretMask(mask: string): void { set("secretMask", mask) },
    endSecret(): void {
      batch(() => {
        set("secretProvider", undefined)
        set("secretMask", "")
      })
    },

    completionContext(): CompletionContext {
      return {
        backendCommands: state.backendCommands.map((command) => command.name),
        providers: [...new Set([...state.providers.map((provider) => provider.id), ...state.savedKeys])],
        savedKeys: state.savedKeys,
        models: state.models.map((model) => model.id),
        sessions: state.sessions.map((session) => session.id),
        workflows: state.workflows.map((workflow) => workflow.name),
        interactions: state.interactions.map((interaction) => interaction.id),
        agents: state.agents.map((agent) => agent.name),
        apiOperations: apiOperationNames,
      }
    },
  }
}

export type AppStore = ReturnType<typeof createAppStore>
