/**
 * The TUI's single state store.
 *
 * It holds a copy of the server projection read over the v1 API (sessions,
 * transcript, interactions, catalogs) plus local UI state (view, status line,
 * concealed key entry). Every field is a Solid signal, so components re-render
 * when a mutation runs; mutations are the only way to change state.
 *
 * Streaming: `fold` (a `TranscriptOverlay`) folds stream frames as they
 * arrive; `flushOverlay()` publishes its snapshot to `state.overlay`. The
 * controller flushes at most once per frame, so fast delta streams do not
 * re-render per chunk. `messages` stays the projection; format.ts merges the
 * two for display.
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
  MemberInfo,
  MessageInfo,
  ModelSummary,
  StreamEvent,
  ProviderSummary,
  SessionInfo,
  TodoItem,
  WorkflowSummary,
} from "../client"
import type { CompletionContext } from "../completion"
import type { View } from "../instructions"
import { toggledSidebar, type SidebarMode } from "./layout"
import { foldMember, type ChildState } from "./members"
import { TranscriptOverlay, type OverlayEffect } from "./overlay"

/** A prompt submitted while a turn runs; sent when the session is free. */
export interface QueuedPrompt {
  id: number
  session: string
  /** The prompt text, or the command of a `!command` shell turn. */
  text: string
  /** A `!command`: sent as a `ShellTurn`. */
  shell?: boolean
  /** `sending` while its CreateTurn is in flight (hidden from the transcript). */
  state: "queued" | "sending"
}

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
  /** Snapshot of the streaming overlay (see overlay.ts), merged over `messages` for display. */
  readonly overlay: MessageInfo[]
  /** Prompts waiting for the running turn to end, oldest first. */
  readonly queued: QueuedPrompt[]
  /** A turn admitted (or being admitted) by this client is running. */
  readonly running: boolean
  /** Turn id (= user message id) of the turn this client admitted, or "". */
  readonly turnId: string
  /** Last durable event sequence applied from the session stream. */
  readonly cursor: string
  readonly view: View
  readonly apiOutput: string
  readonly status: string
  /** Provider whose key is being entered; undefined outside key entry. */
  readonly secretProvider: string | undefined
  readonly secretMask: string
  /** Sidebar mode (state/layout.ts): `auto` follows the terminal width. */
  readonly sidebar: SidebarMode
  /** Terminal width in columns, kept current by the root layout. */
  readonly columns: number
  /** Global reasoning switch (`/thinking`, Ctrl+O): expand every reasoning block. */
  readonly thinking: boolean
  /** Per-part reasoning expansion that overrides `thinking` (mouse click on a Thinking line). */
  readonly reasoningToggles: ReadonlyMap<string, boolean>
  /** Global tool-card switch (`/tools`, Ctrl+G); `undefined` = the defaults (collapsed, shell turns expanded). */
  readonly tools: boolean | undefined
  /** Per-card expansion that overrides `tools` (a click on the card header), by part id. */
  readonly toolToggles: ReadonlyMap<string, boolean>
  /** Subagents of the open session (`SessionInfo.members` + `memberUpdated` frames, folded by member). */
  readonly members: MemberInfo[]
  /** What the controller last read about each child session, by session id. */
  readonly children: ReadonlyMap<string, ChildState>
  /** Bumped when the transcript should jump to its newest line (a prompt was submitted). */
  readonly followTick: number
  /** Commands of the shell turns run from this TUI, by the turn's assistant message id. */
  readonly shellCommands: ReadonlyMap<string, string>
  /** Command of the shell turn running now (its message ids are not known until it returns). */
  readonly pendingShell: string | undefined
  /**
   * `/name args` invocation text of command turns run from this TUI, by the
   * turn's user message id (`CreateTurn` returns the user message id as the
   * turn id for a `CommandTurn`, same as a prompt turn). The transcript shows
   * this instead of the backend's expanded template text.
   */
  readonly commandDisplay: ReadonlyMap<string, string>
  /** Backend version from bootstrap (`/status`). */
  readonly serverVersion: string
  /** The open session's todo list (`/todos`, `GetSessionTodo`). */
  readonly todos: TodoItem[]
  /** Text of the `/status` view. */
  readonly statusText: string
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
    overlay: [],
    queued: [],
    running: false,
    turnId: "",
    cursor: "0",
    view: "chat",
    apiOutput: "Use /api METHOD /v1/path [JSON object] to call any HTTP/JSON endpoint.\n\n" + operations(),
    status: startupStatus,
    secretProvider: undefined,
    secretMask: "",
    sidebar: "auto",
    columns: 80,
    thinking: false,
    reasoningToggles: new Map(),
    tools: undefined,
    toolToggles: new Map(),
    members: [],
    children: new Map(),
    followTick: 0,
    shellCommands: new Map(),
    pendingShell: undefined,
    commandDisplay: new Map(),
    serverVersion: "",
    todos: [],
    statusText: "",
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
  const fold = new TranscriptOverlay()
  let queueIds = 0

  return {
    state,
    /** The streaming fold; mutate it only through `applyEvent`. */
    fold,

    applyBootstrap(bootstrap: Bootstrap): void {
      batch(() => {
        set("agents", bootstrap.agents ?? [])
        set("models", bootstrap.models ?? [])
        set("interactions", bootstrap.interactions ?? [])
        set("serverVersion", bootstrap.location?.version ?? "")
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

    /**
     * Select a session: chat view, empty transcript, stream resumes from its
     * last sequence. The overlay, prompt queue, and turn state belong to the
     * previous session and are dropped.
     */
    openSession(session: SessionInfo): void {
      fold.reset(session.lastSeq ?? "0")
      batch(() => {
        set("selected", session)
        set("view", "chat")
        set("cursor", fold.lastSeq)
        set("messages", [])
        set("overlay", [])
        set("queued", [])
        set("running", false)
        set("turnId", "")
        set("members", session.members ?? [])
        set("children", new Map())
      })
    },

    setSelected(session: SessionInfo): void { set("selected", session) },

    /** A fresh session list (sidebar nesting and `busy` flags); keeps the open session's row current. */
    setSessions(rows: SessionInfo[]): void {
      batch(() => {
        set("sessions", rows)
        const selected = state.selected
        const row = selected && rows.find((candidate) => candidate.id === selected.id)
        if (row) set("selected", { ...selected, ...row })
      })
    },

    /**
     * Store a projection read; ignored (returns false) when another session is
     * selected. Overlay messages the projection shows finished are dropped in
     * the same batch, so the handover never shows a message twice.
     */
    setMessages(sessionId: string, rows: MessageInfo[]): boolean {
      if (state.selected?.id !== sessionId) return false
      fold.prune(rows)
      batch(() => {
        set("messages", rows)
        set("overlay", fold.messages())
      })
      return true
    },

    /** Fold one stream event into the overlay (not yet visible; see `flushOverlay`). */
    applyEvent(event: StreamEvent): OverlayEffect {
      const effect = fold.apply(event)
      if (effect.durable) set("cursor", fold.lastSeq)
      // Members fold here, not in the overlay: they are session state, not transcript parts.
      if (event.memberUpdated?.member && (effect.durable || !event.seq)) set("members", foldMember(state.members, event.memberUpdated))
      return effect
    },

    /** Replace the member rows (a fresh `SessionInfo.members` read). */
    setMembers(rows: MemberInfo[]): void { set("members", rows) },
    /** Record what was read about one child session. */
    setChild(id: string, child: ChildState): void { set("children", new Map(state.children).set(id, child)) },

    /** Publish the overlay's current snapshot. */
    flushOverlay(): void { set("overlay", fold.messages()) },

    /** A `resync` dropped frames: see `TranscriptOverlay.markLiveLost`. */
    markLiveLost(): void { fold.markLiveLost() },

    enqueue(text: string, session: string, shell = false): QueuedPrompt {
      const item: QueuedPrompt = { id: ++queueIds, session, text, ...(shell ? { shell: true } : {}), state: "queued" }
      set("queued", [...state.queued, item])
      return item
    },
    setQueuedState(id: number, value: QueuedPrompt["state"]): void {
      set("queued", state.queued.map((item) => item.id === id ? { ...item, state: value } : item))
    },
    dequeue(id: number): void { set("queued", state.queued.filter((item) => item.id !== id)) },

    setInteractions(rows: Interaction[]): void { set("interactions", rows) },
    setWorkflowState(value: Record<string, unknown> | undefined): void { set("workflowState", value) },
    setView(view: View): void { set("view", view) },
    setStatus(text: string): void { set("status", text) },
    setApiOutput(text: string): void { set("apiOutput", text) },

    setColumns(columns: number): void {
      if (columns !== state.columns) set("columns", columns)
    },
    setSidebar(mode: SidebarMode): void { set("sidebar", mode) },
    /** Show the sidebar if it is hidden at the current width, else hide it. */
    toggleSidebar(): void { set("sidebar", toggledSidebar(state.sidebar, state.columns)) },

    /** Expand or collapse every reasoning block; forgets per-part toggles. */
    setThinking(expanded: boolean): void {
      batch(() => {
        set("thinking", expanded)
        set("reasoningToggles", new Map())
      })
    },
    /** Flip one reasoning block against its current state. */
    toggleReasoning(partId: string): void {
      const next = new Map(state.reasoningToggles)
      next.set(partId, !(state.reasoningToggles.get(partId) ?? state.thinking))
      set("reasoningToggles", next)
    },

    /** Expand (`true`) or collapse every tool card; forgets per-card toggles. */
    setTools(expanded: boolean): void {
      batch(() => {
        set("tools", expanded)
        set("toolToggles", new Map())
      })
    },
    /** Flip one tool card against its current state (`expanded`, as shown now). */
    toggleTool(partId: string, expanded: boolean): void {
      set("toolToggles", new Map(state.toolToggles).set(partId, !expanded))
    },

    /** Remember the command of a shell turn (its assistant message id) for the transcript. */
    rememberShell(messageId: string, command: string): void {
      if (!messageId) return
      set("shellCommands", new Map(state.shellCommands).set(messageId, command))
    },
    /** The shell turn now running (or `undefined` once it returned). */
    setPendingShell(command: string | undefined): void { set("pendingShell", command) },

    /** Remember the `/name args` display text of a command turn, by its user message id. */
    rememberCommand(messageId: string, text: string): void {
      if (!messageId) return
      set("commandDisplay", new Map(state.commandDisplay).set(messageId, text))
    },

    setTodos(items: TodoItem[]): void { set("todos", items) },
    setStatusText(text: string): void { set("statusText", text) },

    /** Ask the transcript to jump to its newest line. */
    followTranscript(): void { set("followTick", state.followTick + 1) },

    /** A prompt is being admitted: the session counts as running from now on. */
    beginTurn(): void {
      batch(() => {
        set("running", true)
        set("turnId", "")
      })
    },

    /** Record the admitted turn id (the user message id); used by /cancel and turn-end matching. */
    setTurn(id: string): void { set("turnId", id) },

    /** The turn ended (final assistant `messageFinished`) or was never admitted. */
    endTurn(): void {
      batch(() => {
        set("running", false)
        set("turnId", "")
      })
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
