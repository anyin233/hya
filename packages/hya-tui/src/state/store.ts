/**
 * The TUI's single state store.
 *
 * It holds a copy of the server projection read over the v1 API (sessions,
 * transcript, interactions, catalogs) plus local UI state (view, status line,
 * the Provider View). Every field is a Solid signal, so components re-render
 * when a mutation runs; mutations are the only way to change state.
 *
 * Streaming: `fold` (a `TranscriptOverlay`) folds stream frames as they
 * arrive; `flushOverlay()` publishes its snapshot to `state.overlay`. The
 * controller flushes at most once per frame, so fast delta streams do not
 * re-render per chunk. `messages` stays the projection; format.ts merges the
 * two for display.
 *
 * The store never holds a secret: the Provider View's key fields keep only a
 * mask length here; the key itself stays in the controller's `SecretEntry`
 * (see completion.ts, state/providers.ts).
 */
import { batch, createSignal, type Accessor, type Setter } from "solid-js"
import { apiOperationNames, operations } from "../api"
import type { VimMode } from "../composer/vim"
import type {
  AgentModelState,
  AgentSummary,
  Bootstrap,
  CommandSummary,
  Interaction,
  McpServerStatus,
  MemberInfo,
  MessageInfo,
  ModelSummary,
  ProjectInfo,
  PromptAttachment,
  StreamEvent,
  ProviderSummary,
  SavedRule,
  SessionInfo,
  TodoItem,
  TokenUsage,
  WorkflowSummary,
} from "../client"
import type { WebInfo } from "../cli"
import type { CompletionContext } from "../completion"
import type { View } from "../instructions"
import type { AgentModelsViewState } from "./agentModels"
import type { DiffViewState } from "./diff"
import { toggledProjectsSidebar, toggledSidebar, type SidebarMode } from "./layout"
import { foldMember, type ChildState } from "./members"
import type { McpViewState } from "./mcp"
import { mergeTranscript, TranscriptOverlay, type OverlayEffect } from "./overlay"
import { mergeInteractions } from "./prompts"
import { compactionText } from "./format"
import { manualMode, modeCycle, modeNotice, type ModeConfirm, type PermissionModeInfo } from "./modes"
import type { ActivePicker, PickerState } from "./picker"
import type { ProviderViewState } from "./providers"
import { sessionRow } from "./revert"
import type { RulesViewState } from "./rules"
import type { ProjectViewState } from "./projectView"

/** A prompt submitted while a turn runs; sent when the session is free. */
export interface QueuedPrompt {
  id: number
  session: string
  /** The prompt text, or the command of a `!command` shell turn. */
  text: string
  /** A `!command`: sent as a `ShellTurn`. */
  shell?: boolean
  /** Resolved `@path` image attachments (composer/attachments.ts), sent with the prompt on `CreateTurn`. */
  attachments?: PromptAttachment[]
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
  /** `GET /v1/providers` (id, protocol, key source, auth, model count): the Provider View's list. */
  readonly providers: ProviderSummary[]
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
  /** `Date.now()` when the running turn was admitted (the working indicator's elapsed clock); `undefined` when none runs. */
  readonly turnStartedAt: number | undefined
  /** Last durable event sequence applied from the session stream. */
  readonly cursor: string
  readonly view: View
  readonly apiOutput: string
  readonly status: string
  /** The full-screen Provider View (`/key`, state/providers.ts), while open. */
  readonly providerView: ProviderViewState | undefined
  /** The full-screen Diff View (`/diff`, state/diff.ts), while open. */
  readonly diffView: DiffViewState | undefined
  /** The full-screen MCP View (`/mcp`, state/mcp.ts), while open. */
  readonly mcpView: McpViewState | undefined
  /** `GetMcpStatus`: every configured MCP server, read when `/mcp` opens. */
  readonly mcpServers: McpServerStatus[]
  /** The full-screen Saved Rules view (`/rules`, state/rules.ts), while open. */
  readonly rulesView: RulesViewState | undefined
  /** `ListSavedRules`: saved permission rules, read when `/rules` opens. */
  readonly savedRules: SavedRule[]
  /** The full-screen Agent Models view (`/agent-models`, state/agentModels.ts), while open. */
  readonly agentModelsView: AgentModelsViewState | undefined
  /** `ListAgentModels`: effective base model of every catalog agent, read when `/agent-models` opens. */
  readonly agentModelRows: AgentModelState[]
  /** Sidebar mode (state/layout.ts): `auto` follows the terminal width. */
  readonly sidebar: SidebarMode
  /** Terminal width in columns, kept current by the root layout. */
  readonly columns: number
  /** Global reasoning switch (`/thinking`, Ctrl+O): expand every reasoning block. */
  readonly thinking: boolean
  /** Vim mode in the composer (`/vim`, the `vim` preference; composer/vim.ts). */
  readonly vim: boolean
  /** The composer's vim mode while `vim` is on. */
  readonly vimMode: VimMode
  /** A half-typed normal-mode command (`2d`, `g`), shown in the status bar. */
  readonly vimPending: string
  /** Desktop notifications on turn end / permission or question asks while unfocused (`/notifications`, the `notifications` preference; src/notify.ts); default on. */
  readonly notifications: boolean
  /** Whether the terminal (or browser tab) is focused, tracked through the terminal's focus reporting (app/run.tsx, app/controller.ts); default true (assume focused until told otherwise). */
  readonly focused: boolean
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
  /** Highlighted option of the shown permission/question prompt, for the ask `id`. */
  readonly promptSelection: { id: string; index: number } | undefined
  /** The composer holds text (a prompt then leaves its keys to the input). */
  readonly draft: boolean
  /** Current git branch of the workspace directory (`GetVcsStatus`); "" when unknown or not a repository. */
  readonly gitBranch: string
  /** The session event stream is connected (status bar "connection state"). */
  readonly connected: boolean
  /** Live `CompactionApplied` events and permission mode switches rendered as transcript notices, oldest first (compactions from before the session was opened are derived from their summary messages at render: state/messages.ts `withDividers`). */
  readonly dividers: readonly Divider[]
  /** Selectable permission modes (`GET /v1/permission-modes`); empty until read or on an older backend. */
  readonly permissionModes: PermissionModeInfo[]
  /** A mode chosen before any session exists; applied right after the session is created (app/modes.ts). */
  readonly pendingMode: string | undefined
  /** A `/model` choice made before any session exists; applied at the next `CreateSession` (C11, app/controller.ts `newSession`). */
  readonly pendingModel: string | undefined
  /** A `/agent` choice made before any session exists; applied at the next `CreateSession` (C12). */
  readonly pendingAgent: string | undefined
  /** The one-line yolo confirmation, while it is shown. */
  readonly modeConfirm: ModeConfirm | undefined
  /** The open modal picker (components/Picker.tsx), if any. */
  readonly picker: ActivePicker | undefined
  /** The open session's newest billed round with a message (`tokensRecorded`), for live context occupancy (E22). */
  readonly liveRound: LiveRound | undefined
  /** The backend this TUI started (one-command launch), for `/status`; `undefined` with `--server`. */
  readonly backend: BackendInfo | undefined
  /** The WebUI bare `hya` serves next to this TUI (`--web-url` / `--web-error`); `undefined` otherwise. */
  readonly web: WebInfo | undefined
  /** `ListProjects` (with `busy` per Project), re-read on `projectsUpdated` (app/controller.ts). */
  readonly projects: ProjectInfo[]
  /**
   * The Project new sessions go to and the directory scope follows
   * (state/projects.ts); `undefined` before one is chosen (`--remote`) or
   * when `EnsureProjectForPath` failed.
   */
  readonly activeProjectId: string | undefined
  /** `--remote`: started without a Project for `--dir`. */
  readonly remote: boolean
  /** Left Projects sidebar mode (state/layout.ts): `auto` follows the terminal width (a wider threshold than the right sidebar). */
  readonly projectsSidebar: SidebarMode
  /** The left sidebar has focus: Up/Down move `projectSidebarHighlight`, Enter switches, Esc returns focus to the composer. */
  readonly projectsSidebarFocus: boolean
  /** Highlighted row of the left sidebar while it has focus (or the active Project, for a first Enter without moving). */
  readonly projectSidebarHighlight: string | undefined
  /** The full-screen Project view (`/project`, `/projects`; state/projectView.ts), while open. */
  readonly projectView: ProjectViewState | undefined
  /** The `/sessions` picker's "all projects" toggle (F3): shows every session instead of only the active Project's. */
  readonly sessionsPickerAllProjects: boolean
}

/** One billed provider round (`tokensRecorded` with a non-empty `message`). */
export interface LiveRound {
  message: string
  /** `provider/model` that served it. */
  model: string
  usage: TokenUsage
}

/** A backend started by this TUI (src/launch.ts). */
export interface BackendInfo {
  pid: number
  /** The `hya` binary this TUI started (absent when attached). */
  bin?: string
  /** The database (absent when bare `hya` attached and did not say). */
  db?: string
  /** The server is another process's that this TUI (or bare `hya`) attached to; quitting does not stop it. */
  attached?: boolean
}

/**
 * A transcript notice (a compaction divider or a mode switch): shown right
 * before `beforeMessageId` when that message is in the transcript (a
 * compaction's summary message), else right after the message that was
 * newest when it happened.
 */
export interface Divider {
  id: string
  text: string
  /** The message that was newest when it happened; `""` = the transcript was empty (shown first); omitted = at the end. */
  afterMessageId?: string
  /** A compaction's summary system message (`compactionApplied.message`): the divider sits right before it. */
  beforeMessageId?: string
}

/** Rows loaded by one full catalog refresh. */
export interface Catalog {
  sessions: SessionInfo[]
  interactions: Interaction[]
  models: ModelSummary[]
  /** `GET /v1/agents`; kept current so the `/agent` picker (C12) reflects catalog changes, not just bootstrap. Omitted keeps the rows read before. */
  agents?: AgentSummary[]
  workflows: WorkflowSummary[]
  providers: ProviderSummary[]
  commands: CommandSummary[]
  /** `GET /v1/permission-modes`; omitted keeps the rows read before. */
  permissionModes?: PermissionModeInfo[]
  /** `GET /v1/projects`; omitted keeps the rows read before. */
  projects?: ProjectInfo[]
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
    backendCommands: [],
    workflows: [],
    workflowState: undefined,
    selected: undefined,
    overlay: [],
    queued: [],
    running: false,
    turnId: "",
    turnStartedAt: undefined,
    cursor: "0",
    view: "chat",
    apiOutput: "Use /api METHOD /v1/path [JSON object] to call any HTTP/JSON endpoint.\n\n" + operations(),
    status: startupStatus,
    providerView: undefined,
    diffView: undefined,
    mcpView: undefined,
    mcpServers: [],
    rulesView: undefined,
    savedRules: [],
    agentModelsView: undefined,
    agentModelRows: [],
    sidebar: "auto",
    columns: 80,
    thinking: false,
    vim: false,
    vimMode: "insert",
    vimPending: "",
    notifications: true,
    focused: true,
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
    promptSelection: undefined,
    draft: false,
    gitBranch: "",
    connected: true,
    dividers: [],
    permissionModes: [],
    pendingMode: undefined,
    pendingModel: undefined,
    pendingAgent: undefined,
    modeConfirm: undefined,
    picker: undefined,
    liveRound: undefined,
    backend: undefined,
    web: undefined,
    projects: [],
    activeProjectId: undefined,
    remote: false,
    projectsSidebar: "auto",
    projectsSidebarFocus: false,
    projectSidebarHighlight: undefined,
    projectView: undefined,
    sessionsPickerAllProjects: false,
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
  /** Asks as their live frames carried them (the listing omits a question's options), by id. */
  const liveAsks = new Map<string, Interaction>()
  /** Asks answered from this TUI: hidden even from a listing read before the answer landed. */
  const answered = new Set<string>()
  const upsertAsk = (interaction: Interaction): void => {
    if (!interaction.id || answered.has(interaction.id)) return
    liveAsks.set(interaction.id, interaction)
    const rows = state.interactions
    const at = rows.findIndex((row) => row.id === interaction.id)
    set("interactions", at < 0 ? [...rows, interaction] : rows.map((row, index) => index === at ? { ...row, ...interaction } : row))
  }
  /** The mode the transcript last announced (or the opened session's mode): a switch to it adds no second notice. */
  let noticedMode = manualMode
  let noticeIds = 0
  const addNotice = (text: string, id: string, beforeMessageId?: string): void => {
    if (state.dividers.some((divider) => divider.id === id)) return
    const afterMessageId = mergeTranscript(state.messages, fold.messages()).at(-1)?.id ?? ""
    set("dividers", [...state.dividers, { id, text, afterMessageId, ...(beforeMessageId ? { beforeMessageId } : {}) }])
  }
  /** The open session's tree now runs in `mode`: update the session row and announce a change once. */
  const applyPermissionMode = (mode: string): void => {
    const selected = state.selected
    if (!selected || !mode) return
    batch(() => {
      if (selected.permissionMode !== mode) set("selected", { ...selected, permissionMode: mode })
      if (mode !== noticedMode) {
        noticedMode = mode
        addNotice(modeNotice(mode, state.permissionModes), `mode-${++noticeIds}`)
      }
    })
  }
  /** `"provider/model[#variant]"` → `SessionInfo.model`; empty/unparsable stays unset. */
  const parseModelRef = (reference: string): SessionInfo["model"] | undefined => {
    const [providerId, rest] = reference.split(/\/(.+)/, 2)
    if (!providerId || !rest) return undefined
    const [modelId, variant] = rest.split("#", 2)
    return { providerId, modelId, ...(variant ? { variant } : {}) }
  }
  /** A `sessionUpdated` title/agent/model frame (G30): patch the row in `sessions` and, if it is the open one, `selected` too. */
  const applySessionMeta = (sessionId: string, patch: { title?: string; agent?: string; model?: string }): void => {
    const changes: Partial<SessionInfo> = {}
    if (patch.title !== undefined) changes.title = patch.title
    if (patch.agent !== undefined) changes.agent = patch.agent
    if (patch.model !== undefined) {
      const model = parseModelRef(patch.model)
      if (model) changes.model = model
    }
    if (!Object.keys(changes).length) return
    batch(() => {
      if (state.sessions.some((row) => row.id === sessionId)) {
        set("sessions", state.sessions.map((row) => row.id === sessionId ? { ...row, ...changes } : row))
      }
      if (state.selected?.id === sessionId) set("selected", { ...state.selected, ...changes })
    })
  }
  const dropAsk = (id: string): void => {
    liveAsks.delete(id)
    if (state.interactions.some((row) => row.id === id)) set("interactions", state.interactions.filter((row) => row.id !== id))
  }
  /** A live ask frame (`permissionRequested` / `questionRequested` / `interactionResolved`), the open session's or a descendant's. */
  const applyAsk = (event: StreamEvent): void => {
    const asked = event.permissionRequested?.interaction ?? event.questionRequested?.interaction
    if (asked) upsertAsk({ ...asked, ...(asked.session ? {} : event.session ? { session: event.session } : {}), type: asked.type || (event.questionRequested ? "INTERACTION_TYPE_QUESTION" : "INTERACTION_TYPE_PERMISSION") })
    if (event.interactionResolved?.request) dropAsk(event.interactionResolved.request)
  }

  return {
    state,
    /** The streaming fold; mutate it only through `applyEvent`. */
    fold,

    applyBootstrap(bootstrap: Bootstrap): void {
      batch(() => {
        set("agents", bootstrap.agents ?? [])
        set("models", bootstrap.models ?? [])
        set("interactions", mergeInteractions(bootstrap.interactions ?? [], liveAsks, answered))
        set("serverVersion", bootstrap.location?.version ?? "")
      })
    },

    applyCatalog(catalog: Catalog): void {
      batch(() => {
        set("sessions", catalog.sessions)
        set("interactions", mergeInteractions(catalog.interactions, liveAsks, answered))
        set("models", catalog.models)
        if (catalog.agents) set("agents", catalog.agents)
        set("workflows", catalog.workflows)
        set("providers", catalog.providers)
        set("backendCommands", catalog.commands)
        if (catalog.permissionModes) set("permissionModes", catalog.permissionModes)
        if (catalog.projects) set("projects", catalog.projects)
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
      noticedMode = session.permissionMode || manualMode
      batch(() => {
        set("selected", session)
        set("view", "chat")
        set("cursor", fold.lastSeq)
        set("messages", [])
        set("overlay", [])
        set("queued", [])
        set("running", false)
        set("turnId", "")
        set("turnStartedAt", undefined)
        set("members", session.members ?? [])
        set("children", new Map())
        set("dividers", [])
        set("liveRound", undefined)
        // The sidebar's session list may hold a stale `busy` (the last list
        // read, possibly before or after this session's turn ended): sync its
        // row to this fresh read so the row does not show `running` forever.
        if (state.sessions.some((row) => row.id === session.id)) {
          set("sessions", state.sessions.map((row) => row.id === session.id ? sessionRow(row, session) : row))
        }
      })
    },

    /**
     * The open session's `busy` flag changed (a turn on its own stream ended,
     * `docs/tui.md` "Sidebar"): keep the sidebar row current without waiting
     * for the next full session list refresh.
     */
    setSessionBusy(id: string, busy: boolean): void {
      if (!state.sessions.some((row) => row.id === id && row.busy !== busy)) return
      set("sessions", state.sessions.map((row) => row.id === id ? { ...row, busy } : row))
    },

    setSelected(session: SessionInfo): void { set("selected", session) },

    /**
     * The open session after a revert or redo (`RevertSession`'s `session`):
     * its row (with or without `revert`) replaces the open one, and the
     * streaming overlay is dropped — no turn runs during a revert, and the
     * overlay's messages may be the hidden ones (the projection re-read
     * shows what is left).
     */
    applyRevert(session: SessionInfo): void {
      const selected = state.selected
      if (selected?.id !== session.id) return
      fold.reset(fold.lastSeq)
      batch(() => {
        set("selected", sessionRow(selected, session))
        set("overlay", [])
        if (state.sessions.some((row) => row.id === session.id)) {
          set("sessions", state.sessions.map((row) => row.id === session.id ? sessionRow(row, session) : row))
        }
      })
    },

    /** The open session's tree runs in `mode` now (a switch or a `sessionUpdated` frame); adds the transcript notice once per change. */
    applyPermissionMode,
    setPermissionModes(rows: PermissionModeInfo[]): void { set("permissionModes", rows) },
    setPendingMode(mode: string | undefined): void { set("pendingMode", mode) },
    /** A `/model` choice made before any session exists (C11); `undefined` clears it (applied or cancelled). */
    setPendingModel(model: string | undefined): void { set("pendingModel", model) },
    /** A `/agent` choice made before any session exists (C12); `undefined` clears it. */
    setPendingAgent(agent: string | undefined): void { set("pendingAgent", agent) },
    /** No session is open (after deleting the open one with none left to switch to). */
    clearSelected(): void { set("selected", undefined) },
    setModeConfirm(confirm: ModeConfirm | undefined): void { set("modeConfirm", confirm) },
    /** Show a modal picker (or replace the open one's state); `undefined` closes it. */
    setPicker(picker: ActivePicker | undefined): void { set("picker", picker) },
    /** Replace the open picker's list state, keeping its selection callback. */
    updatePicker(next: PickerState): void {
      const open = state.picker
      if (open) set("picker", { ...open, ...next })
    },

    /** A fresh session list (sidebar nesting and `busy` flags); keeps the open session's row current. */
    setSessions(rows: SessionInfo[]): void {
      batch(() => {
        set("sessions", rows)
        const selected = state.selected
        const row = selected && rows.find((candidate) => candidate.id === selected.id)
        if (row) set("selected", sessionRow(selected, row))
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
      // Pending asks are live frames: shown at once.
      applyAsk(event)
      // Durable session state below: skip a replayed duplicate (effect.durable is false for it).
      // A revert or redo (this client's, or another's): the overlay may hold hidden messages; the controller re-reads the rest.
      if (event.sessionReverted && effect.durable) {
        fold.reset(fold.lastSeq)
        set("overlay", [])
      }
      // The next message after a revert commits it: `/redo` is no longer possible.
      const selected = state.selected
      if (event.messageStarted && effect.durable && selected?.revert && (!event.session || event.session === selected.id)) {
        const { revert: _committed, ...rest } = selected
        set("selected", rest)
      }
      const compaction = event.compactionApplied
      if (compaction && effect.durable) addNotice(compactionText(compaction), `divider-${event.seq}`, compaction.message)
      // The whole list after a todo tool changed it (E23).
      if (event.todoUpdated && effect.durable) set("todos", event.todoUpdated.items ?? [])
      // A billed round of a message: the live context occupancy source (E22). Side calls have no message.
      const tokens = event.tokensRecorded
      if (tokens?.message && tokens.usage && effect.durable) set("liveRound", { message: tokens.message, model: tokens.model ?? "", usage: tokens.usage })
      // The tree's mode changed (another client, or this one's own switch echoed): the root's stream carries it.
      const mode = event.sessionUpdated?.permissionMode
      if (mode && (!event.session || event.session === state.selected?.id)) applyPermissionMode(mode)
      // A title/agent/model change (a `/rename`, `/agent`, `/model` from elsewhere, or the backend's
      // auto-generated title after the first turn, G30): keep the header, sidebar, and `/sessions`
      // picker current without waiting for the next catalog refresh.
      const updated = event.sessionUpdated
      if (updated && event.session && (updated.title !== undefined || updated.agent !== undefined || updated.model !== undefined)) {
        applySessionMeta(event.session, updated)
      }
      return effect
    },

    /**
     * A descendant's (subagent's) ask frame from the open session's stream
     * (`includeDescendants=true`): only the pending list changes; its
     * transcript is not folded into this session's.
     */
    applyAsk,
    /** The backend this TUI started (`/status`). */
    setBackend(info: BackendInfo | undefined): void { set("backend", info) },
    /** The WebUI state from bare `hya` (status bar, sidebar, `/status`). */
    setWeb(info: WebInfo | undefined): void { set("web", info) },

    /** Replace the member rows (a fresh `SessionInfo.members` read). */
    setMembers(rows: MemberInfo[]): void { set("members", rows) },
    /** Record what was read about one child session. */
    setChild(id: string, child: ChildState): void { set("children", new Map(state.children).set(id, child)) },

    /** Publish the overlay's current snapshot. */
    flushOverlay(): void { set("overlay", fold.messages()) },

    /** A `resync` dropped frames: see `TranscriptOverlay.markLiveLost`. */
    markLiveLost(): void { fold.markLiveLost() },

    enqueue(text: string, session: string, shell = false, attachments?: PromptAttachment[]): QueuedPrompt {
      const item: QueuedPrompt = {
        id: ++queueIds, session, text, ...(shell ? { shell: true } : {}), ...(attachments?.length ? { attachments } : {}), state: "queued",
      }
      set("queued", [...state.queued, item])
      return item
    },
    setQueuedState(id: number, value: QueuedPrompt["state"]): void {
      set("queued", state.queued.map((item) => item.id === id ? { ...item, state: value } : item))
    },
    dequeue(id: number): void { set("queued", state.queued.filter((item) => item.id !== id)) },

    /** A fresh listing, merged with what live frames carried; asks answered here stay hidden. */
    setInteractions(rows: Interaction[]): void { set("interactions", mergeInteractions(rows, liveAsks, answered)) },
    /** An ask was answered from this TUI: hide it now (the answer is in flight). */
    resolveInteraction(id: string): void {
      answered.add(id)
      dropAsk(id)
    },
    /** The answer failed: let the next listing show the ask again. */
    unresolveInteraction(id: string): void { answered.delete(id) },
    /** Highlighted option of the prompt for ask `id` (0 for any other ask). */
    promptIndex(id: string): number {
      const selection = state.promptSelection
      return selection?.id === id ? selection.index : 0
    },
    setPromptIndex(id: string, index: number): void { set("promptSelection", { id, index }) },
    setDraft(value: boolean): void {
      if (value !== state.draft) set("draft", value)
    },
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

    /** Turn vim mode on (starting in insert mode) or off. */
    setVim(on: boolean): void {
      batch(() => {
        set("vim", on)
        set("vimMode", "insert")
        set("vimPending", "")
      })
    },
    /** The composer's vim mode and half-typed command (components/Composer.tsx). */
    setVimMode(mode: VimMode, pending = ""): void {
      batch(() => {
        set("vimMode", mode)
        set("vimPending", pending)
      })
    },

    /** Turn desktop notifications on or off (`/notifications`). */
    setNotifications(on: boolean): void { set("notifications", on) },
    /** The terminal's (or browser tab's) focus state, from the renderer's focus reporting. */
    setFocused(focused: boolean): void { set("focused", focused) },

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

    /** Current git branch of the workspace (`GetVcsStatus`); "" when unknown or not a repository. */
    setGitBranch(branch: string): void {
      if (branch !== state.gitBranch) set("gitBranch", branch)
    },
    /** The session event stream's connection state (status bar). */
    setConnected(value: boolean): void {
      if (value !== state.connected) set("connected", value)
    },

    /** Ask the transcript to jump to its newest line. */
    followTranscript(): void { set("followTick", state.followTick + 1) },

    /** A prompt is being admitted: the session counts as running from now on. */
    beginTurn(): void {
      batch(() => {
        set("running", true)
        set("turnId", "")
        set("turnStartedAt", Date.now())
      })
    },

    /** Record the admitted turn id (the user message id); used by /cancel and turn-end matching. */
    setTurn(id: string): void { set("turnId", id) },

    /** The turn ended (final assistant `messageFinished`) or was never admitted. */
    endTurn(): void {
      batch(() => {
        set("running", false)
        set("turnId", "")
        set("turnStartedAt", undefined)
      })
    },

    /** Open, update, or (`undefined`) close the Provider View. */
    setProviderView(view: ProviderViewState | undefined): void { set("providerView", view) },
    /** A catalog refresh (`GET /v1/providers`, `GET /v1/models`) without the rest of `applyCatalog`. */
    setProviderCatalog(providers: ProviderSummary[], models: ModelSummary[]): void {
      batch(() => {
        set("providers", providers)
        set("models", models)
      })
    },

    /** Open, update, or (`undefined`) close the Diff View (`/diff`). */
    setDiffView(view: DiffViewState | undefined): void { set("diffView", view) },

    /** Open, update, or (`undefined`) close the MCP View (`/mcp`). */
    setMcpView(view: McpViewState | undefined): void { set("mcpView", view) },
    setMcpServers(servers: McpServerStatus[]): void { set("mcpServers", servers) },

    /** Open, update, or (`undefined`) close the Saved Rules view (`/rules`). */
    setRulesView(view: RulesViewState | undefined): void { set("rulesView", view) },
    setSavedRules(rules: SavedRule[]): void { set("savedRules", rules) },

    /** Open, update, or (`undefined`) close the Agent Models view (`/agent-models`). */
    setAgentModelsView(view: AgentModelsViewState | undefined): void { set("agentModelsView", view) },
    setAgentModelRows(rows: AgentModelState[]): void { set("agentModelRows", rows) },

    /** `ListProjects` rows (after a `projectsUpdated` frame or a Project write). */
    setProjects(rows: ProjectInfo[]): void { set("projects", rows) },
    /** The Project new sessions go to; `undefined` = none chosen. */
    setActiveProject(id: string | undefined): void {
      if (id !== state.activeProjectId) set("activeProjectId", id)
    },
    setRemote(value: boolean): void { set("remote", value) },

    setProjectsSidebar(mode: SidebarMode): void { set("projectsSidebar", mode) },
    toggleProjectsSidebar(): void { set("projectsSidebar", toggledProjectsSidebar(state.projectsSidebar, state.columns)) },
    setProjectsSidebarFocus(focus: boolean): void { set("projectsSidebarFocus", focus) },
    setProjectSidebarHighlight(id: string | undefined): void { set("projectSidebarHighlight", id) },

    /** Open, update, or (`undefined`) close the full-screen Project view (`/project`). */
    setProjectView(view: ProjectViewState | undefined): void { set("projectView", view) },
    setSessionsPickerAllProjects(value: boolean): void { set("sessionsPickerAllProjects", value) },

    completionContext(): CompletionContext {
      return {
        backendCommands: state.backendCommands.map((command) => command.name),
        models: state.models.map((model) => model.id),
        sessions: state.sessions.map((session) => session.id),
        workflows: state.workflows.map((workflow) => workflow.name),
        interactions: state.interactions.map((interaction) => interaction.id),
        agents: state.agents.map((agent) => agent.name),
        apiOperations: apiOperationNames,
        permissionModes: modeCycle(state.permissionModes),
      }
    },
  }
}

export type AppStore = ReturnType<typeof createAppStore>
