/**
 * The controller coordinates session navigation, command and prompt submission,
 * backend lifecycle (backendLifecycle.ts), and streams (subscriptions.ts).
 * Components read the store and call controller methods.
 *
 * The TUI reads the server projection (sessions, transcript, interactions).
 * Stream frames are folded into the store's transient streaming overlay
 * (state/overlay.ts), published at most once per display frame, and
 * durable/interaction frames trigger a debounced projection re-read; the
 * projection stays authoritative. Prompt admission and the prompt queue
 * live in turns.ts.
 *
 * Stream lifecycle: subscribe (`sinceSeq` = last applied durable seq), then
 * gap-fill with `ListEvents` before reading frames, on every (re)connect.
 * A `resync` marks interrupted live parts and gap-fills again. Opening a
 * session aborts the old stream and resets the overlay.
 *
 * Subagents: the open session's child sessions (its members and `task`
 * outputs; state/members.ts) are re-read — `GetSession` for `busy`,
 * `ListMessages` for the latest activity — after every projection read and
 * member frame, and every `childPollMs` while a child is busy or this
 * client's turn runs. That is the only polling left: it feeds the task
 * cards' status and activity (durable child events stay on the child's own
 * stream).
 *
 * Prompts: the open session's stream is subscribed with
 * `includeDescendants=true`, so the live `permissionRequested` /
 * `questionRequested` / `interactionResolved` frames of the open session and
 * of every subagent below it update the pending list at once (state/store.ts
 * `applyAsk`; state/prompts.ts `askFrameRoute`). Frames sent before the
 * subscription are not replayed, so `GET /v2/interactions` is read once
 * after every (re)subscribe and after a `resync`, besides the full catalog
 * refreshes (start, /refresh). `answer()` responds to a prompt
 * (app/prompts.ts).
 *
 * Asks of other sessions: from start, the global stream
 * (`GET /v2/events/stream`, subscribed past every durable seq so only
 * live frames arrive) feeds the ask and resolve frames of every session
 * into the pending list at once (state/prompts.ts `globalAskRoute`); an ask
 * of a session outside the open tree also sets a status notice naming the
 * session and sends a desktop notification. The listing is re-read on every
 * (re)subscribe and `resync`; the stream reconnects with a backoff. The
 * open tree's asks arrive on both streams and are kept by id, and each ask
 * notifies at most once.
 *
 * Usage and todos: `tokensRecorded`, `todoUpdated`, and `compactionApplied`
 * fold in the store; a `tokensRecorded` also re-reads the open session
 * (debounced) for its authoritative `SessionInfo.usage` total.
 *
 * Projects (ADR-0024, state/projects.ts): a local start ensures the Project
 * containing `--dir` (`EnsureProjectForPath`) and makes it active; `--remote`
 * starts with none. New sessions go to the active Project (working in
 * `--dir` when it lies inside, else the primary root), or are temporary.
 * `switchProject` moves the active Project, the client's directory scope,
 * and the open session together; opening a root session of another Project
 * makes that Project active. The Project list (with `busy`) is re-read on
 * the global stream's `projectsUpdated` frames, debounced.
 *
 * Losing the server (app/reconnect.ts): when either stream fails or ends
 * and the server fails its health probes, a TUI that knows its database
 * (`reconnect` set) finds or starts the next server, switches the client's
 * base URL, resubscribes both streams, and reloads the catalogs and the open
 * session. A turn that ran on the old server died with it. A server that
 * says why it stops (`serverStopping`, the last frame of each stream)
 * changes that: after `stop` nothing is started and prompts are refused
 * until `/reconnect`; after `restart` the TUI waits for the next server.
 *
 * Sessions (app/sessionKeeper.ts): a plain local start resumes the active
 * Project's latest durable session, preferring one with a waiting request;
 * with no history a new `ephemeral` session is created (as is every `/new`
 * one): the daemon deletes it while still unused once no client watches it
 * (no session stream open on it), so leaving it (another session opened, any
 * exit, a kill) needs no request. `close("archive")` (a graceful exit:
 * `/exit`, Ctrl+C twice) archives the open session's root when it is used;
 * `background` (`/to-background`, Ctrl+D) and `signal` leave it running.
 * `/resume` and `--resume` unarchive and open one (app/resume.ts).
 */
import { SessionViews } from "./sessionViews"
import { type HyaClient, type Interaction, type ProjectInfo, type PromptAttachment, type CommandSummary, type SessionInfo, type SkillListing } from "../client"
import { completeCommand } from "../completion"
import { createCommandRegistry, mergeCommandEntries, openModelPicker, selectAgent, toBackground, type AppActions, type CommandEntry, type CommandRegistry } from "../commands"
import {
  attachmentName,
  exceedsTurnBudget,
  imageMentionPaths,
  maxAttachmentBytes,
  maxTurnAttachmentBytes,
  mimeForPath,
  spacedImageError,
  spacedImageMentions,
  validateAttachmentBytes,
  type AttachmentPreview,
} from "../composer/attachments"
import { findPattern, rankPaths } from "../composer/mention"
import { ChipTable } from "../composer/chips"
import type { PastedPath } from "../composer/paths"
import { resolvePastedPaths, type PasteResult } from "./paste"
import { inSshSession, readClipboard, type ClipboardPaste } from "../composer/hostClipboard"
import { prepareImage } from "../composer/images"
import type { Chip } from "../composer/chips"
import { shellCommand } from "../composer/shell"
import type { KeyLike } from "../keys/bindings"
import { helpPickerHint, helpPickerRows } from "../commands"
import { createBackendLifecycle } from "./backendLifecycle"
import { currentModel, modelReference } from "../state/format"
import { childSessionIds } from "../state/members"
import { editText } from "../composer/editor"
import { loadPaneLayout, savePreferences, type TuiPreferences } from "../prefs"
import type { StructureControl } from "./structure"
import { notificationBody, notificationSequence, shouldNotify, type NotifyKind } from "../notify"
import { createPicker, pickerHighlighted, pickerKey as pickerKeyOutcome, type PickerRow, type PickerSpec } from "../state/picker"
import { treeSessionIds, type PromptChoice } from "../state/prompts"
import { activeProject, newestTopLevelSession, noProjectStatus, projectScope, sessionPlacement } from "../state/projects"
import { extensionManager, type CommandOrigin } from "../extensions/manager"
import type { HostCommand, JsonValue } from "../extensions/wire"
import { projectsSidebarVisible } from "../state/layout"
import { paneLeaves } from "../state/panes"
import { createAgentsViewController } from "./agentsView"
import type { UiHandles } from "./context"
import { createDiffController } from "./diff"
import { createMcpController } from "./mcp"
import { createBundlesController } from "./bundles"
import { createModeSwitcher } from "./modes"
import { setExtensionTrust } from "../commands/native"
import { createProviderController } from "./providers"
import { answerPrompt } from "./prompts"
import { revertReadOnlyStatus as readOnlyStatus, createRevertController } from "./revert"
import { createRulesController } from "./rules"
import { createSkillsController } from "./skills"
import { createReloader } from "./reload"
import type { AppStore } from "../state/store"
import { errorLine } from "../state/projectCommands"
import { createSubscriptions } from "./subscriptions"
import { createTurnRunner } from "./turns"
import type { ServerSwitch } from "./reconnect"
import { createSessionKeeper, type ExitMode } from "./sessionKeeper"
import { createResumer } from "./resume"
import { containsRelayLink, parseConnectRemote, redactRelayLinks, type Bridge, type BridgeFlags } from "../bridge"
import { SecretEntry } from "../completion"

/** Longest wait for the session stream before a prompt is admitted anyway. */
const streamWaitMs = 3000

/** Longest wait on exit for archiving the open session (a graceful exit of a used session only). */
const archiveOnExitMs = 2_000
export interface ControllerOptions {
  client: HyaClient
  store: AppStore
  /** Workspace directory (`--dir`): the Project ensured at a local start, and the workdir of new sessions inside it. */
  directory: string
  /** `--remote`: no `EnsureProjectForPath` at start; new sessions need a chosen Project (or are temporary). */
  remote?: boolean
  registry?: CommandRegistry
  /** Leave the TUI (destroys the renderer, which restores the terminal); `mode` is what happens to the open session (app/sessionKeeper.ts). */
  quit?: (mode: "archive" | "background") => void
  /** Which session to open at start (`--continue`, `--session`, `--resume [id]`; src/launch.ts `initialSessionId`, app/resume.ts). Default: a new one. */
  startup?: { continue: boolean; session?: string; resume?: { id?: string } }
  /** Appended to the status line when the backend cannot be reached. */
  connectionHint?: string
  /** TUI preferences file (src/prefs.ts); unset = preference changes apply for this run only. */
  preferencesPath?: string
  /** Reload the backend TUI extension catalog after bundle changes. */
  refreshExtensions?: () => Promise<void>
  /** Saved default for newly created sessions; existing sessions retain their backend mode. */
  preferredPermissionMode?: string
  /** The terminal behind the renderer (app/run.tsx): clipboard and handing it to an external editor. */
  terminal?: TerminalAccess
  /** Environment for `$VISUAL` / `$EDITOR` (default `process.env`). */
  env?: Record<string, string | undefined>
  /**
   * Find or start the database's server after this one went away
   * (app/reconnect.ts; src/launch.ts `connectOrStart`). Unset for a fixed
   * `--server` without `--db`: the streams just keep retrying it.
   */
  reconnect?: () => Promise<ServerSwitch>
  /** Find the database's running server without starting one (src/launch.ts `findRunningServer`): how a stopped TUI notices a server another client started, and how it follows `hya serve restart`. */
  find?: () => Promise<ServerSwitch | undefined>
  /** Called once the TUI followed `hya serve restart` to the successor (app/reconnect.ts `onRestarted`): app/run.tsx reloads the TUI's code then. */
  onRestarted?: () => void
  /** Health probe of a server URL (default src/launch.ts `probeHealth`, with the client's bridge token for its own URL). */
  probe?: (url: string) => Promise<boolean>
  /**
   * `/connect-remote`: start a relay bridge child for `link` (app/run.tsx:
   * `hya bridge`, src/bridge.ts `startBridge`) and resolve once it is ready;
   * `onLine` gets its status lines. Unset: `/connect-remote` is unavailable.
   */
  bridge?: (link: string, flags: BridgeFlags, onLine: (line: string) => void) => Promise<Bridge>
  /**
   * `/disconnect-remote`: the local backend to go back to (the database's
   * daemon, found or started, or a fixed `--server`). Unset when this TUI has
   * none (started by bare `hya --connect`).
   */
  home?: () => Promise<ServerSwitch>
  /** The backend scope changed (server, Project, or temporary session directory): reload the bundle TUI extensions (app/run.tsx). */
  onScopeChanged?: () => void | Promise<void>
  /** `/reload-tui`: restart this TUI from its files through the launcher's supervisor (src/reload.ts); absent without one. */
  reloadTui?: () => void
  /** Bundle TUI extension submit interceptors (extensions/manager.ts `interceptSubmit`): a prompt may be rewritten or blocked before it is sent. */
  interceptSubmit?: (text: string) => Promise<{ text: string } | { blocked: string }>
  /** `tui.toml` and the session's `/set` (app/structure.ts); unset in tests: the store's structure stays. */
  structure?: StructureControl
}

/** What the controller needs from the renderer (CliRenderer in app/run.tsx; a fake in tests). */
export interface TerminalAccess {
  /** Write `text` as an OSC 52 clipboard sequence; `false` when the terminal does not accept it. */
  copy(text: string): boolean
  /** Give the terminal to a child process (CliRenderer.suspend). */
  suspend(): void
  /** Take it back and repaint (CliRenderer.resume). */
  resume(): void
  /** Write a pre-built escape sequence (src/notify.ts `notificationSequence`) straight to the terminal. */
  notify?(sequence: string): void
  /** Subscribe to the terminal's focus reporting (CliRenderer "focus"/"blur"); returns the unsubscribe function. Absent when the renderer has none (tests). */
  onFocusChange?(handler: (focused: boolean) => void): () => void
}

/** The composer's input, registered by components/Composer.tsx for the external editor. */
export interface ComposerAccess {
  text(): string
  /** Replace the input (cursor at the end); not sent. */
  setText(text: string): void
}

/** Rows the help overlay shows at once (bounded by the terminal height, components/Picker.tsx). */
const helpMaxRows = 40

/** One attachment file read: its size and (within the per-file cap) its base64 bytes, or why it could not be read. */
type FileRead = { size: number; data?: string } | { error: string }

/** Paths requested per `@file` lookup; the best `fileSuggestionLimit` are shown. */
const fileLookupLimit = 50
export const fileSuggestionLimit = 8

export function createController({ client, store, directory, remote: startedRemote = false, registry = createCommandRegistry(), quit = () => undefined, startup = { continue: false }, connectionHint = "start hya serve", preferencesPath, preferredPermissionMode, terminal, env = process.env, reconnect, find, onRestarted, refreshExtensions, probe, bridge: startRemoteBridge, home, onScopeChanged, interceptSubmit, structure, reloadTui }: ControllerOptions) {
  const sessionViews = new SessionViews(client)

  /** Asks a desktop notification was considered for: the open tree's asks arrive on both streams. */
  const notifiedAsks = new Set<string>()

  let unsubscribeFocus: (() => void) | undefined
  /** `/connect-remote` without a link: the concealed entry's text (never in the store). */
  const secret = new SecretEntry()
  let secretFlags: BridgeFlags = {}
  /** The server as the user should see it: a label (the remote) replaces the loopback bridge URL. */
  function shownServerText(text: string): string {
    const label = store.state.serverLabel
    if (!label) return text
    const url = (client.baseUrl ?? "").replace(/\/+$/, "")
    return url ? text.split(url).join(label) : text
  }
  /** Status line text never carries the loopback bridge URL (a label replaces it) or a relay link's secret. */
  const status = (text: string): void => store.setStatus(redactRelayLinks(shownServerText(text)))

  const backend = createBackendLifecycle({
    client, store, directory, startedRemote, startup, connectionHint, reconnect, find, onRestarted, probe,
    startRemoteBridge, home, onScopeChanged, status, isClosing: () => subscriptions.closing, refresh, refreshVcs,
    subscribeFocus: () => { unsubscribeFocus = terminal?.onFocusChange?.((focused) => store.setFocused(focused)) },
    startGlobalStream: () => subscriptions.startGlobalStream(), startStream: (id) => subscriptions.startStream(id),
    stopGlobalStream: () => subscriptions.stopGlobalStream(), leaveOpenSession,
    activateProject, openProjectOverlay, openSession, newSession, resume: (id, onCancel) => resumer.resume(id, onCancel),
  })

  /** Desktop notifications (src/notify.ts): only while unfocused and the preference is on. */
  function sendNotification(kind: NotifyKind, detail: string): void {
    if (!terminal?.notify) return
    if (!shouldNotify({ notifications: store.state.notifications, focused: store.state.focused })) return
    terminal.notify(notificationSequence(notificationBody(kind, detail)))
  }

  /** Notify about one ask at most once, however many streams carry it. */
  function notifyAsk(id: string, kind: "permission" | "question", detail: string): void {
    if (!id || notifiedAsks.has(id)) return
    notifiedAsks.add(id)
    sendNotification(kind, detail)
  }

  const turns = createTurnRunner({
    store,
    client,
    onEnd: (outcome) => {
      // The terminal title's `✓`/`✗` until the terminal is focused again (src/title.ts).
      if (!store.state.focused) store.setUnseenOutcome(outcome.ok ? "finished" : "failed")
      sendNotification(outcome.ok ? "turnFinished" : "turnFailed", outcome.ok ? (store.state.selected?.title ?? "") : outcome.detail)
    },
  })
  const subscriptions = createSubscriptions({
    store, client, backend, turns, status, notifyAsk, refreshProjects, refreshVcs, newSession,
  })
  const { refreshMessages, scheduleRefresh, startStream, deleteSession } = subscriptions
  const modes = createModeSwitcher({
    store,
    client,
    preferredMode: preferredPermissionMode,
    saveMode: preferencesPath ? (mode) => savePreferences(preferencesPath, { permissionMode: mode }) : undefined,
  })
  const keeper = createSessionKeeper({
    client: {
      getSession: (id) => client.request<SessionInfo>("GET", `/v2/sessions/${encodeURIComponent(id)}`),
      archiveSession: async (id) => { await client.setArchived(id, true) },
    },
  })

  /**
   * `ListSkills` and `ListCommands` into the store: the Skills view, the
   * startup list's `[Skills]`, and the command menu's `/skill:*` rows read
   * them (`/reload-skills`, `/reload-commands`, the Skills view's `r`).
   */
  async function readSkills(): Promise<SkillListing> {
    const session = store.state.selected?.id
    const [listing] = await Promise.all([client.listSkills(session), readCommands()])
    store.setSkills(listing)
    return listing
  }

  /**
   * `ListCommands` (with its diagnostics) into the store, for the open
   * session when there is one: the command menu then lists the `/skill:*`
   * rows of its agent's bundle skills too.
   */
  async function readCommands(): Promise<CommandSummary[]> {
    const session = store.state.selected?.id
    const listing = client.listCommandCatalog
      ? await client.listCommandCatalog(session)
      : { commands: await client.listCommands(), diagnostics: [] }
    store.setBackendCommands(listing.commands)
    store.setCommandDiagnostics(listing.diagnostics)
    return listing.commands
  }

  async function refresh(): Promise<void> {
    const [sessions, interactions, models, agents, workflows, providers, commands, permissionModes, projects, skills] = await Promise.all([
      client.listSessions(), client.listInteractions(), client.listModels(), client.listAgents(),
      // Optional: absent when `hya/workflow` is not loaded (e.g. `hya --bare`).
      client.listWorkflows().catch(() => [] as Awaited<ReturnType<typeof client.listWorkflows>>),
      client.listProviders(),
      // The open session's catalog (its agent's bundle skills too), with the command diagnostics.
      client.listCommandCatalog
        ? client.listCommandCatalog(store.state.selected?.id).then((listing) => { store.setCommandDiagnostics(listing.diagnostics); return listing.commands })
        : client.listCommands(),
      // Optional: the Shift+Tab cycle falls back to the built-ins without it.
      client.listPermissionModes().catch(() => undefined),
      // Optional: a failed read keeps the rows read before.
      client.listProjects().catch(() => undefined),
      // Optional: the skills and their diagnostics (an older backend without them keeps the bootstrap's).
      (client.listSkills?.(store.state.selected?.id) ?? Promise.resolve(undefined)).catch(() => undefined),
    ])
    store.applyCatalog({ sessions, interactions, models, agents, workflows, providers, commands, ...(permissionModes ? { permissionModes } : {}), ...(projects ? { projects } : {}) })
    if (skills) store.setSkills(skills)
  }

  /** `ListProjects` into the store (the Project list and its `busy` flags); a failure is kept as `projectsError` (the Project view shows it) and thrown. */
  async function refreshProjects(): Promise<void> {
    try {
      store.setProjects(await client.listProjects())
    } catch (error) {
      store.setProjectsError(errorLine(error))
      throw error
    }
  }

  async function refreshTodos(): Promise<void> {
    const selected = store.state.selected
    if (!selected) return
    store.setTodos(await client.getSessionTodo(selected.id).catch(() => store.state.todos))
  }

  /** `GetVcsStatus` for the status bar's git branch (E22) and extensions' git context; refreshed on session open and after turns. */
  async function refreshVcs(): Promise<void> {
    store.setVcs(await client.getVcsStatus().catch(() => undefined))
  }

  async function openSession(sessionId: string): Promise<void> {
    const listed = store.state.sessions.find((row) => row.id === sessionId)
    // A fresh read gives the current `lastSeq`, so the stream gap-fill stays small.
    const session = await client.request<SessionInfo>("GET", `/v2/sessions/${encodeURIComponent(sessionId)}`)
      .catch((error: unknown) => {
        if (listed) return listed
        throw error
      })
    followSessionScope(session)
    subscriptions.resetChildren()
    store.openSession(session)
    await refreshMessages()
    void refreshTodos()
    void refreshVcs()
    // Leaving `previous` closes its session stream: an unused ephemeral one is the daemon's to drop.
    startStream(session.id)
  }

  /** Make `project` active and scope the client to it (`--dir` when inside, else the primary root). */
  function activateProject(project: ProjectInfo): void {
    if (!store.state.projects.some((row) => row.id === project.id)) store.setProjects([project, ...store.state.projects])
    store.setActiveProject(project.id)
    client.setDirectory(projectScope(project, directory, backend.remote))
    void onScopeChanged?.()
  }

  /**
   * An opened root session carries its Project: a Project session makes its
   * (listed) Project active; a temporary session scopes the client to its
   * scratch workdir and leaves the active Project for the next `/new`.
   * Subagent sessions change nothing.
   */
  function followSessionScope(session: SessionInfo): void {
    if (session.parent) return
    if (session.kind === "SESSION_KIND_TEMPORARY") {
      if (session.workdir) { client.setDirectory(session.workdir); void onScopeChanged?.() }
      return
    }
    const project = session.projectId ? store.state.projects.find((row) => row.id === session.projectId) : undefined
    if (project) activateProject(project)
  }

  /**
   * Make Project `id` active: scope the client to it, then open its most
   * recently updated root session, or create one there when it has none
   * (which restarts the session stream either way).
   */
  async function switchProject(id: string): Promise<void> {
    const project = store.state.projects.find((row) => row.id === id) ?? await client.getProject(id)
    activateProject(project)
    const target = newestTopLevelSession(await client.listSessions({ projectId: project.id }), project.id)
    if (target) await openSession(target.id)
    else await newSession()
    status(`Project ${project.name} · ${target ? "opened its latest session" : "new session"}`)
  }

  /** Leave a subagent's read-only view: open its parent session. */
  async function returnToParent(): Promise<void> {
    const parent = store.state.selected?.parent
    if (!parent) return
    await openSession(parent)
    status("Back to the parent session")
  }

  /**
   * `CreateSession` and open it: in the active Project (state/projects.ts
   * `sessionPlacement`), or temporary. Without an active Project a
   * `--remote` start refuses (status `noProjectStatus`, `NoProjectError`).
   */
  async function newSession(agentArg?: string, modelArg?: string, options: { temporary?: boolean } = {}): Promise<void> {
    const placement = sessionPlacement({ project: activeProject(store.state), directory, remote: backend.remote, ...(options.temporary ? { temporary: true } : {}) })
    if (!placement) {
      status(noProjectStatus)
      // Without a Project to place it in (a `--remote` start), open the
      // Project view instead of leaving only the status line to explain it.
      openProjectOverlay()
      throw new NoProjectError()
    }
    // Leave agent/model empty unless the user explicitly chose one. The server then
    // resolves config.default_agent and that agent's configured model. This keeps a
    // hot `/agent` switch local to the current session and restores the configured
    // default agent on the next startup.
    const agent = agentArg ?? store.state.pendingAgent ?? ""
    const model = modelArg || store.state.pendingModel || ""
    // Ephemeral: dropped by the daemon while unused once nobody watches it (app/sessionKeeper.ts).
    const session = await client.createSession(agent, model, placement, { ephemeral: true })
    keeper.created(session.id)
    store.setPendingAgent(undefined)
    store.setPendingModel(undefined)
    // Creation already returns the authoritative projection. Avoid the old full
    // catalog refresh here (sessions, models, providers, and bundles); update
    // only the session index and let the normal live stream refresh catalogs.
    store.upsertSession(session)
    await openSession(session.id)
    status(`Created ${session.id}`)
    // A mode chosen before any session existed applies before the first prompt is admitted.
    await modes.applyPending()
  }

  /** Open the modal picker; keys go to it (components/Composer.tsx) until a row is chosen, a row action commits, or Esc closes it. */
  function openPicker(spec: PickerSpec): void {
    store.setPicker({
      ...createPicker(spec),
      onSelect: spec.onSelect,
      ...(spec.onAction ? { onAction: spec.onAction } : {}),
      ...(spec.onHighlight ? { onHighlight: spec.onHighlight } : {}),
      ...(spec.onCancel ? { onCancel: spec.onCancel } : {}),
    })
  }

  /** Open the context actions for one session row (mouse right-click). */
  function openSessionContext(id: string, point?: { x: number; y: number }): void {
    const session = store.state.sessions.find((row) => row.id === id)
    if (!session) return
    openPicker({
      title: `Session · ${session.title || id}`,
      rows: point
        ? [{ id: "open", label: "Open" }, ...(session.archived ? [] : [{ id: "archive", label: "Archive" }]), { id: "delete", label: "Delete" }]
        : [{ id, label: session.title || id, name: session.title || id, detail: session.archived ? "archived" : "active" }],
      hint: point ? "Click an action · Esc closes" : "Enter opens · F2 renames · F3 archives · Ctrl+D deletes · Esc closes",
      ...(point ? { contextMenu: point } : {}),
      actions: [
        { id: "rename", key: "f2", label: "F2 rename", prompt: "value" },
        { id: "archive", key: "f3", label: "F3 archive", prompt: "confirm", confirmText: "Archive {label}? Enter confirms" },
        { id: "delete", key: "d", ctrl: true, label: "Ctrl+D delete", prompt: "confirm", confirmText: "Delete {label}? Enter confirms" },
      ],
      onSelect: (row) => {
        if (point) {
          if (row.id === "open") void openRootSession(id).catch((error: unknown) => status(`Open failed: ${errorLine(error)}`))
          else if (row.id === "archive") void client.setArchived(id, true).then(refresh).then(() => status("Session archived")).catch((error: unknown) => status(`Archive failed: ${errorLine(error)}`))
          else if (row.id === "delete") void deleteSession(id).then(refresh).then(() => status("Session deleted")).catch((error: unknown) => status(`Delete failed: ${errorLine(error)}`))
          return
        }
        void openRootSession(row.id).catch((error: unknown) => status(`Open failed: ${errorLine(error)}`))
      },
      onAction: (action, row, value) => {
        if (action === "rename") void client.updateSession(row.id, { title: value ?? "" }).then(refresh).then(() => status("Session renamed")).catch((error: unknown) => status(`Rename failed: ${errorLine(error)}`))
        else if (action === "archive") void client.setArchived(row.id, true).then(refresh).then(() => status("Session archived")).catch((error: unknown) => status(`Archive failed: ${errorLine(error)}`))
        else if (action === "delete") void deleteSession(row.id).then(refresh).then(() => status("Session deleted")).catch((error: unknown) => status(`Delete failed: ${errorLine(error)}`))
      },
    })
  }

  /** Open the context actions for one Project row (mouse right-click). */
  function openProjectContext(id: string, point?: { x: number; y: number }): void {
    const project = store.state.projects.find((row) => row.id === id)
    if (!project) return
    openPicker({
      title: `Project · ${project.name}`,
      rows: point ? [{ id: "open", label: "Open" }, { id: "delete", label: "Delete" }] : [{ id, label: project.name, name: project.name }],
      hint: point ? "Click an action · Esc closes" : "Enter opens · F2 renames · Ctrl+D deletes · Esc closes",
      ...(point ? { contextMenu: point } : {}),
      actions: [
        { id: "rename", key: "f2", label: "F2 rename", prompt: "value" },
        { id: "delete", key: "d", ctrl: true, label: "Ctrl+D delete", prompt: "confirm", confirmText: "Delete {label}? Enter confirms" },
      ],
      onSelect: (row) => { if (point) { if (row.id === "open") void switchProject(id).catch((error: unknown) => status(`Switch failed: ${errorLine(error)}`)); else if (row.id === "delete") void client.deleteProject(id).then(refreshProjects).then(() => status("Project deleted")).catch((error: unknown) => status(`Delete failed: ${errorLine(error)}`)); return } void switchProject(row.id).catch((error: unknown) => status(`Switch failed: ${errorLine(error)}`)) },
      onAction: (action, row, value) => {
        if (action === "rename") void client.updateProject(row.id, { name: value ?? "" }).then(refreshProjects).then(() => status("Project renamed")).catch((error: unknown) => status(`Rename failed: ${errorLine(error)}`))
        else if (action === "delete") void client.deleteProject(row.id).then(refreshProjects).then(() => status("Project deleted")).catch((error: unknown) => status(`Delete failed: ${errorLine(error)}`))
      },
    })
  }

  /** Remove the picker; focus returns to the composer. */
  function dismissPicker(): void {
    store.setPicker(undefined)
  }

  /** Close the picker without a choice (Esc, Ctrl+C): runs its `onCancel` (undoing a live preview). */
  function closePicker(): void {
    const open = store.state.picker
    dismissPicker()
    open?.onCancel?.()
  }

  /** Choose a picker row (Enter or a click): close first, then run the choice. */
  function choosePickerRow(row: PickerRow): void {
    const open = store.state.picker
    if (!open) return
    dismissPicker()
    void Promise.resolve()
      .then(() => open.onSelect(row))
      .catch((error: unknown) => status(`Error: ${String(error)}`))
  }

  /** Commit a row action (rename/delete, F2/Ctrl+D): close first, then run it. */
  function commitPickerAction(id: string, row: PickerRow, value?: string): void {
    const open = store.state.picker
    if (!open?.onAction) return
    dismissPicker()
    void Promise.resolve()
      .then(() => open.onAction!(id, row, value))
      .catch((error: unknown) => status(`Error: ${String(error)}`))
  }

  /** One key while the picker is open (it takes every key but Ctrl+C). */
  function pickerKey(key: KeyLike): void {
    const open = store.state.picker
    if (!open) return
    const outcome = pickerKeyOutcome(open, key)
    if (outcome.type === "update") {
      const before = pickerHighlighted(open)
      store.updatePicker(outcome.state)
      const after = pickerHighlighted(outcome.state)
      if (after && after.id !== before?.id) open.onHighlight?.(after)
    }
    else if (outcome.type === "close") closePicker()
    else if (outcome.type === "select") choosePickerRow(outcome.row)
    else if (outcome.type === "commit") commitPickerAction(outcome.id, outcome.row, outcome.value)
  }

  /** The key and command help overlay (G29): the picker over `helpPickerRows`, filterable, Esc closes. */
  function openHelp(): void {
    openPicker({
      title: "Help · keys and commands",
      rows: helpPickerRows(commandEntries()),
      hint: helpPickerHint,
      maxRows: helpMaxRows,
      detailPane: true,
      onSelect: () => undefined,
    })
  }

  let composer: ComposerAccess | undefined
  let editing = false

  /**
   * `/editor`: the input in the external editor
   * (composer/editor.ts). The edited text replaces the input (not sent); on
   * a failure the input keeps its text and the status line says why.
   */
  function openEditor(): void {
    const input = composer
    if (!input || !terminal) {
      status("External editor unavailable: no terminal")
      return
    }
    if (editing) return
    editing = true
    const original = input.text()
    void editText(original, { env, suspend: () => terminal.suspend(), resume: () => terminal.resume() })
      .then((result) => {
        if (!result.ok) {
          status(`${result.error} · input unchanged`)
          return
        }
        input.setText(result.text)
        status(result.text === original ? "Editor closed · input unchanged" : "Edited in the external editor · Enter sends")
      })
      .finally(() => { editing = false })
  }

  const providers = createProviderController({
    store,
    client,
    refresh,
    pickModel: (provider, onChosen) => openModelPicker({ store, client, actions }, { title: `Model · pick one of ${provider}'s models for this session`, highlight: provider, onChosen }),
  })
  /** Imperative handles registered by mounted components (the transcript's and Diff view's scroll actions); shared with the AppContext `ui` prop (app/run.tsx). */
  const ui: UiHandles = {}
  const diffView = createDiffController({ store, client, ui })
  const mcp = createMcpController({ store, client, copyText: (text) => terminal?.copy(text) ?? false })
  // Without a catalog refresher (no run.tsx: tests) there is no extension catalog to reload.
  const bundles = createBundlesController({ store, client, directory, refreshExtensions: refreshExtensions ?? (() => Promise.resolve()), savePreferences: (patch) => { if (preferencesPath) savePreferences(preferencesPath, patch) } })
  /**
   * Ctrl+P: focus the left Projects pane, showing it first when it is hidden.
   * Its keys go to the panel drawing it (hya/basic-tui-components, or a bundle
   * replacing it; components/Composer.tsx).
   */
  function focusProjects(): void {
    if (!extensionManager.hasSurface("projects")) {
      status("Projects sidebar unavailable: no TUI bundles loaded")
      return
    }
    // The single-column presets have no Projects pane: show the workspace for this session.
    if (!store.state.structure.sidebar.enabled && structure) structure.setEnabled("sidebar", true)
    if (!paneLeaves(store.state.paneLayout.root).some((pane) => pane.kind === "projects")) {
      status("No Projects pane · /layout split left projects to add one")
      return
    }
    if (!projectsSidebarVisible(store.state.projectsSidebar, store.state.columns)) store.setProjectsSidebar("open")
    store.setProjectsFocus(true)
  }

  /** Open the Project view (`/project`), drawn by the `project_view` extension; at startup, once that extension runs. */
  async function openProjectOverlay(): Promise<void> {
    await extensionManager.settled()
    const panel = extensionManager.replacement("project_view")
    if (!panel) {
      status(extensionManager.placeholder("project_view") ?? "Projects are loading…")
      return
    }
    store.setExtensionOverlay(panel.key)
    // A failure reaches the view as `projects.error` (`Refresh failed: …`).
    void refreshProjects().catch(() => undefined)
  }

  /**
   * A host command an extension panel answered with (extensions/manager.ts;
   * docs/tui-extensions.md "Host commands"). What it returns is the token's
   * result value; what it throws, the result's error.
   */
  async function hostCommand(command: HostCommand, origin: CommandOrigin): Promise<JsonValue | undefined> {
    switch (command.command) {
      case "session.open": await openRootSession(command.id); return undefined
      case "session.menu": openSessionContext(command.id, origin.point); return undefined
      case "session.new_temporary": await actions.newTemporarySession(); return undefined
      case "project.switch": await switchProject(command.id); return undefined
      case "project.menu": openProjectContext(command.id, origin.point); return undefined
      case "project.create": {
        const project = await client.createProject({ name: command.name, roots: [...command.roots] })
        await refreshProjects()
        return { id: project.id, name: project.name }
      }
      case "project.rename": await client.updateProject(command.id, { name: command.name }); await refreshProjects(); return undefined
      case "project.set_roots": await client.updateProject(command.id, { roots: [...command.roots] }); await refreshProjects(); return undefined
      case "project.delete": await client.deleteProject(command.id); await refreshProjects(); return undefined
      case "fs.complete": {
        // A root path being typed: the backend filesystem's first match (`/v2/fs/find`).
        const candidates = await client.findFiles(`${command.input.replace(/\/+$/, "")}*`, 1)
        return { candidates, ...(candidates[0] ? { completion: candidates[0] } : {}) }
      }
      case "ui.release": case "ui.close": case "ui.open": return undefined
    }
  }

  const rules = createRulesController({ store, client })
  const reloader = createReloader({
    store, client, structure, preferencesPath, refreshExtensions, reloadTui,
    readSkills: () => readSkills(),
    readCommands: () => readCommands(),
  })
  const skills = createSkillsController({
    store,
    readSkills: () => readSkills(),
    fillInput: (text) => composer?.setText(text),
    copyText: (text) => terminal?.copy(text) ?? false,
  })
  const agentsView = createAgentsViewController({ store, client, openPicker, selectAgent: (agent) => selectAgent({ store, client, actions }, agent) })
  const revert = createRevertController({ store, client, composer: () => composer, openSession, refresh, openPicker })
  const resumer = createResumer({
    store, openSession, openPicker,
    client: { listSessions: (options) => client.listSessions(options), setArchived: (id, archived) => client.setArchived(id, archived) },
    refresh: async () => { store.setSessions(await client.listSessions()) },
  })

  /** The oldest waiting request outside the open tree: /pending opens its root session so the normal prompt can answer it. */
  async function reviewPending(): Promise<void> {
    const pending = store.state.interactions.find((item) => item.session && !treeSessionIds(store.state.selected?.id ?? "", store.state.sessions, childSessionIds(store.state.members, store.state.messages)).has(item.session))
    if (!pending?.session) { status("No pending request in another session"); return }
    const first = await client.request<SessionInfo>("GET", `/v2/sessions/${encodeURIComponent(pending.session)}`)
    const root = await keeper.rootOf(first)
    await resumer.resume(root)
  }

  /**
   * Merge `patch` into `tui.json`. Only the `workspace` structure owns
   * `paneLayout` (ADR-0036 D10): the chat column of the other presets is
   * never written over the saved tree.
   */
  function savePreferencesPatch(patch: Partial<TuiPreferences>): void {
    if (!preferencesPath) return
    let next = patch
    if ("paneLayout" in next && !store.state.structure.sidebar.enabled) {
      const { paneLayout: _chat, ...rest } = next
      next = rest
    }
    if (Object.keys(next).length) savePreferences(preferencesPath, next)
  }

  const actions: AppActions = {
    reviewPending, refresh, refreshMessages, openSession, newSession, scheduleRefresh, openHelp, openEditor,
    newTemporarySession: (agent, model) => newSession(agent, model, { temporary: true }),
    switchProject,
    refreshProjects,
    openProviders: () => providers.open(),
    openDiff: () => diffView.open(),
    openMcp: () => mcp.open(),
    openBundles: () => bundles.open(),
    openRules: () => rules.open(),
    openAgents: () => agentsView.open(),
    openSkills: (filter) => skills.open(filter),
    openProjectOverlay: () => openProjectOverlay(),
    copyText: (text) => terminal?.copy(text) ?? false,
    undo: () => revert.undo(),
    redo: () => revert.redo(),
    fork: () => revert.fork(),
    reconnect: () => backend.reconnectNow(),
    connectRemote: (args) => connectRemoteCommand(args),
    disconnectRemote: () => backend.disconnectRemote(),
    cancelTurn: () => turns.cancel(),
    sendPrompt: (text) => submit(text, "message"),
    reload: (targets) => reloader.run(targets),
    quit,
    resume: (id) => resumer.resume(id),
    openPicker,
    requestPermissionMode: (mode) => modes.request(mode),
    savePreferences: (patch) => savePreferencesPatch(patch),
    structure,
    expandChip: (index) => ui.expandChip?.(index) ?? false,
    pasteClipboard: (raw) => ui.pasteClipboard?.(raw) ?? Promise.resolve(false),
    paneBounds: () => new Map([...ui.paneBounds ?? []].map(([id, read]) => [id, read()])),
    loadPaneLayout: () => {
      if (!preferencesPath) throw new Error("No TUI preferences path configured")
      return { layout: loadPaneLayout(preferencesPath), path: preferencesPath }
    },
    deleteSession,
  }

  /**
   * Remote mode: pasted (dragged) absolute image paths that exist on this
   * machine. They are the user's local files: read here and sent inline,
   * never looked up on the backend.
   */
  const localPastes = new Set<string>()

  /** Paste and attachment chips of the composer (composer/chips.ts); kept for the process, so history entries keep their tokens. */
  const chips = new ChipTable()

  /** What pasted paths become (app/paste.ts): chips, mentions, or text with a notice. */
  function pastePaths(paths: readonly PastedPath[]): Promise<PasteResult> {
    return resolvePastedPaths(paths, {
      chips,
      remote: backend.remote,
      workdir: store.state.selected?.workdir || directory,
      roots: activeProject(store.state)?.roots ?? [],
      existsOnBackend: (path) => {
        const scope = backendScope(path)
        return scope ? client.readFile(path, { directory: scope, maxBytes: 1 }).then(() => true, () => false) : Promise.resolve(false)
      },
      resizeImages: store.state.structure.composer.paste.image_resize,
      ssh: inSshSession(env),
    })
  }

  /** Ctrl+V, `/paste`, an empty bracketed paste: the system clipboard (composer/hostClipboard.ts); never over SSH. */
  function clipboard(): Promise<ClipboardPaste> {
    return readClipboard(env)
  }

  /** An image chip for clipboard bytes, named `clipboard-<hash>.<ext>`, converted or scaled when needed. */
  async function clipboardImage(bytes: Uint8Array): Promise<Chip> {
    const prepared = await prepareImage(bytes, { resize: store.state.structure.composer.paste.image_resize })
    const hash = new Bun.CryptoHasher("sha256").update(bytes).digest("hex").slice(0, 8)
    const extension = ({ "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp" } as Record<string, string>)[prepared.image?.mime ?? ""] ?? "png"
    return chips.addImage(`clipboard-${hash}.${extension}`, prepared.image, prepared.error)
  }
  /** Remote-mode previews per server + scope + path: typing re-runs the preview, the file is fetched once (cleared on submit). */
  const remotePreviews = new Map<string, Promise<FileRead>>()
  const remotePreviewLimit = 16

  /** Where the backend resolves `path` in remote mode: the active Project's root holding an absolute path, else the open session's workdir, else the client's scope; `undefined` before a Project is chosen. */
  function backendScope(path: string): string | undefined {
    if (path.startsWith("/")) {
      const root = activeProject(store.state)?.roots.find((row) => {
        const base = row.replace(/\/+$/, "")
        return path === base || path.startsWith(`${base}/`)
      })
      if (root) return root
    }
    return store.state.selected?.workdir || client.directory || undefined
  }

  /** One file on this machine (`absolute`); no bytes when over the per-file cap. */
  async function readLocalFile(absolute: string): Promise<FileRead> {
    const file = Bun.file(absolute)
    if (!(await file.exists())) return { error: "file not found" }
    if (file.size > maxAttachmentBytes) return { size: file.size }
    const bytes = new Uint8Array(await file.arrayBuffer())
    return { size: bytes.byteLength, data: Buffer.from(bytes).toString("base64") }
  }

  /** One file on the backend (`ReadFile` under `backendScope`), at most the per-file cap + 1 byte. */
  async function readBackendFile(path: string): Promise<FileRead> {
    const directory = backendScope(path)
    if (!directory) return { error: "choose a project first (in remote mode @path names a file on the backend)" }
    try {
      const result = await client.readFile(path, { directory, maxBytes: maxAttachmentBytes + 1 })
      return result.size > maxAttachmentBytes ? { size: result.size } : { size: result.size, data: result.data }
    } catch (error) {
      const text = String(error)
      if (text.includes("path not found")) return { error: "file not found" }
      if (text.includes("escapes the directory scope")) return { error: "outside the project on the backend" }
      return { error: text }
    }
  }

  /**
   * Resolve every `@path` image mention in `text` to a `PromptAttachment`
   * candidate and check the per-file and per-turn size caps
   * (composer/attachments.ts, docs/protocol/README.md "Prompt attachments
   * (images)"). Local mode reads the file here (relative to the open
   * session's workdir, else `--dir`). Remote mode reads it from the backend
   * (`ReadFile`: relative to the open session's workdir, an absolute path
   * inside the active Project's roots), except a pasted image that exists on
   * this machine (`localPastes`), which is read here. Never throws: a file
   * that cannot be read or fails validation gets `error` set instead of
   * `data`, so the caller can show it without sending. Also used by the
   * composer to preview pending attachments as the user types
   * (components/Composer.tsx; `preview` reuses earlier remote reads).
   */
  async function loadAttachments(text: string, preview = false): Promise<AttachmentPreview[]> {
    const paths = imageMentionPaths(text)
    const spaced = spacedImageMentions(text).filter((item) => !paths.includes(item.path))
    if (!paths.length && !spaced.length) return []
    const workdir = (store.state.selected?.workdir || directory).replace(/\/+$/, "")
    const sizes: number[] = []
    const previews: AttachmentPreview[] = []
    for (const path of paths) {
      const name = attachmentName(path)
      try {
        const read = backend.remote && !localPastes.has(path)
          ? await (preview ? previewBackendFile(path) : readBackendFile(path))
          : await readLocalFile(path.startsWith("/") ? path : `${workdir}/${path}`)
        if ("error" in read) {
          previews.push({ path, name, error: read.error })
          continue
        }
        const sizeError = validateAttachmentBytes(path, read.size)
        if (sizeError || read.data === undefined) {
          previews.push({ path, name, size: read.size, error: sizeError ?? `${name}: could not be read` })
          continue
        }
        if (exceedsTurnBudget(sizes, read.size)) {
          previews.push({ path, name, size: read.size, error: "attachments over 20 MiB for this turn" })
          continue
        }
        sizes.push(read.size)
        previews.push({ path, name, mime: mimeForPath(path), size: read.size, data: read.data })
      } catch (error) {
        previews.push({ path, name, error: String(error) })
      }
    }
    // `@/tmp/shot 1.png` without quotes names `/tmp/shot`: when the joined path is an
    // existing image, say so and refuse to send rather than silently attaching nothing.
    for (const item of spaced) {
      if (await fileExists(item.path).catch(() => false)) previews.push({ path: item.path, name: attachmentName(item.path), error: spacedImageError(item.path) })
    }
    return previews
  }

  /** `readBackendFile` for the composer's preview: one fetch per server, scope, and path until the next submit. */
  function previewBackendFile(path: string): Promise<FileRead> {
    const key = `${client.baseUrl}\n${backendScope(path) ?? ""}\n${path}`
    let read = remotePreviews.get(key)
    if (!read) {
      if (remotePreviews.size >= remotePreviewLimit) remotePreviews.clear()
      read = readBackendFile(path)
      remotePreviews.set(key, read)
    }
    return read
  }

  /** Submit text from a named input surface. `auto` preserves programmatic callers. */
  async function submit(value: string, source: "auto" | "message" | "command" = "auto"): Promise<void> {
    const text = value.trim()
    if (!text) return
    try {
      // A relay link is a credential: only `/connect-remote` takes it, nothing else sends it anywhere.
      if (containsRelayLink(text) && !(source !== "message" && /^\/connect-remote(\s|$)/.test(text))) {
        status(relayLinkRefusedStatus)
        return
      }
      if (source === "command" || (source === "auto" && text.startsWith("/"))) {
        if (!text.startsWith("/")) throw new Error("Command must start with /")
        await registry.dispatch(text, { store, client, actions })
        return
      }
      // A prompt typed before startup has opened its session belongs to that session; creating one
      // here as well would leave the prompt in a session startup then navigates away from.
      if (!store.state.selected) await backend.starting
      if (store.state.selected?.parent) {
        status(readOnlyStatus)
        return
      }
      if (backend.reconnectStopped() && !backend.viaBridge()) {
        status(stoppedPromptStatus)
        return
      }
      if (backend.bridgeDown) {
        status(bridgeDownPromptStatus)
        return
      }
      const command = shellCommand(text)
      if (command === "") throw new Error("Usage: !<shell command>")
      if (!store.state.selected) await newSession()
      keeper.used(store.state.selected!.id)
      if (command !== undefined) {
        store.followTranscript()
        await Promise.race([subscriptions.streamReady, Bun.sleep(streamWaitMs)])
        await turns.submit(command, { shell: true })
        return
      }
      let prompt = text
      if (interceptSubmit) {
        const intercepted = await interceptSubmit(text)
        if ("blocked" in intercepted) {
          status(`Not sent · ${intercepted.blocked}`)
          return
        }
        prompt = intercepted.text
      }
      remotePreviews.clear()
      // `@path` mentions are read from the text as typed: never from inside a pasted chip.
      const previews = await loadAttachments(prompt)
      const chipErrors = chips.present(prompt).filter((chip) => chip.error)
      const failed = [...previews.filter((item) => item.error), ...chipErrors.map((chip) => ({ name: chip.name ?? chip.token, error: chip.error }))]
      if (failed.length) {
        status(`Not sent · ${failed.map((item) => `${item.name}: ${item.error}`).join(" · ")}`)
        return
      }
      const expanded = chips.expand(prompt, { wrap: store.state.structure.composer.paste.wrap })
      const images = [...expanded.images.map((item) => ({ name: item.name, mime: item.mime, data: item.data, path: item.path })), ...previews.map((item) => ({ name: item.name, mime: item.mime ?? "", data: item.data ?? "", path: item.path }))]
      if (images.length && currentModel(store.state)?.imageInput === false) {
        status(`Not sent · ${modelReference(store.state.selected!)} does not accept image attachments`)
        return
      }
      const total = expanded.chips.reduce((sum, chip) => sum + (chip.image?.size ?? 0), 0) + previews.reduce((sum, item) => sum + (item.size ?? 0), 0)
      if (total > maxTurnAttachmentBytes) {
        status("Not sent · attachments over 20 MiB for this turn")
        return
      }
      const attachments: PromptAttachment[] = images
      store.followTranscript()
      // Subscribe before CreateTurn, so no frame of the new turn is missed.
      await Promise.race([subscriptions.streamReady, Bun.sleep(streamWaitMs)])
      await turns.submit(expanded.text, attachments.length ? { attachments } : {})
    } catch (error) {
      // The refusal already says what to do.
      if (!(error instanceof NoProjectError)) status(`Error: ${String(error)}`)
    }
  }

  /** Esc: cancel the running turn. */
  function cancelTurn(): void {
    void turns.cancel().catch((error: unknown) => status(`Cancel failed: ${String(error)}`))
  }

  /** Answer a permission or question prompt; the pending list and transcript are re-read after. */
  function answer(interaction: Interaction, choice: PromptChoice): void {
    void answerPrompt({ store, client }, interaction, choice).then(() => {
      if (choice.kind !== "other") scheduleRefresh()
    })
  }

  /** `@file` suggestions: paths under `--dir` containing `query`, best first. */
  async function findFiles(query: string): Promise<string[]> {
    return rankPaths(await client.findFiles(findPattern(query), fileLookupLimit), query, fileSuggestionLimit)
  }

  /**
   * Whether a pasted image path exists — it becomes an `@path ` mention only
   * when it does (components/Composer.tsx). Local mode: on this machine
   * (relative to the open session's workdir, else `--dir`). Remote mode: an
   * absolute path that exists on this machine is the user's local file
   * (remembered in `localPastes`, read here and sent inline); anything else
   * is checked on the backend (`ReadFile` of one byte under `backendScope`).
   */
  async function fileExists(path: string): Promise<boolean> {
    if (backend.remote) {
      if (path.startsWith("/") && await Bun.file(path).exists().catch(() => false)) {
        localPastes.add(path)
        return true
      }
      const scope = backendScope(path)
      if (!scope) return false
      return client.readFile(path, { directory: scope, maxBytes: 1 }).then(() => true, () => false)
    }
    const workdir = (store.state.selected?.workdir || directory).replace(/\/+$/, "")
    const absolute = path.startsWith("/") ? path : `${workdir}/${path}`
    try {
      return await Bun.file(absolute).exists()
    } catch {
      return false
    }
  }

  function refreshAll(): void {
    void refresh().then(refreshMessages).catch((error: unknown) => status(`Refresh failed: ${String(error)}`))
  }

  /**
   * Leave the open session of the server being left: closing its session
   * stream is all it takes, since an unused session this client created is
   * ephemeral and that server's daemon drops it once nobody watches it.
   */
  async function leaveOpenSession(): Promise<void> {
    subscriptions.stopSessionStream()
    store.closeSession()
  }

  /** `/connect-remote` without a link: a concealed entry replaces the composer (the text stays in `secret`). */
  function openSecretEntry(flags: BridgeFlags): void {
    secret.clear()
    secretFlags = flags
    store.setSecretEntry({ title: "Relay link", length: 0, hint: secretEntryHint })
    // The entry's own hint says what to do; a completion hint from typing the command is stale.
    status("")
  }

  function closeSecretEntry(): void {
    secret.clear()
    secretFlags = {}
    store.setSecretEntry(undefined)
  }

  /** One key while the concealed entry is open: printable keys append, Backspace deletes, Enter connects, Esc cancels. */
  function secretKey(key: KeyLike): void {
    if (!store.state.secretEntry) return
    if (key.name === "escape") {
      closeSecretEntry()
      status("Not connected · /connect-remote cancelled")
      return
    }
    if ((key.name === "return" || key.name === "kpenter") && !key.meta) {
      const link = secret.take()
      const flags = secretFlags
      closeSecretEntry()
      if (!link) {
        status("Not connected · no relay link was entered")
        return
      }
      void backend.connectRemote(link, flags)
      return
    }
    if (key.name === "backspace") secret.backspace()
    else if (!key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f") secret.append(key.sequence)
    else return
    store.setSecretEntry({ ...store.state.secretEntry, length: secret.length })
  }

  /** A paste while the concealed entry is open. */
  function secretPaste(text: string): void {
    if (!store.state.secretEntry) return
    secret.append(text)
    store.setSecretEntry({ ...store.state.secretEntry, length: secret.length })
  }

  /** `/connect-remote [<link>] [flags]` (commands/native.ts). */
  async function connectRemoteCommand(args: readonly string[]): Promise<void> {
    const { link, flags } = parseConnectRemote(args)
    if (link === undefined) openSecretEntry(flags)
    else await backend.connectRemote(link, flags)
  }

  /**
   * Before exit: on a graceful exit (`archive`) archive the open session's
   * root when it is used (bounded wait). An unused session this client
   * created costs no request and no wait: the daemon drops it once this
   * client's stream is gone. `background` and `signal` (the default: a WebUI
   * tab closed, a kill) leave it running on the daemon.
   */
  async function close(mode: ExitMode = "signal"): Promise<void> {
    const selected = store.state.selected?.id
    if (selected && mode === "archive" && !keeper.isFresh(selected)) {
      await Promise.race([keeper.leave(selected, mode).catch(() => undefined), Bun.sleep(archiveOnExitMs)])
    }
    await backend.stopBridge()
    dispose()
  }

  function dispose(): void {
    sessionViews.dispose()
    subscriptions.dispose()
    providers.dispose()
    diffView.dispose()
    mcp.dispose()
    bundles.dispose()
    rules.dispose()
    skills.dispose()
    agentsView.dispose()
    unsubscribeFocus?.()
    secret.clear()
  }

  /** Open a session's top-level owner; used by mouse navigation in the session tab. */
  function openRootSession(sessionId: string): Promise<void> {
    const byId = new Map(store.state.sessions.map((session) => [session.id, session]))
    let session = byId.get(sessionId)
    const seen = new Set<string>()
    while (session?.parent && !seen.has(session.id)) {
      seen.add(session.id)
      session = byId.get(session.parent)
    }
    return openSession((session ?? byId.get(sessionId))?.id ?? sessionId)
  }

  /** Merged, deduplicated suggestions for the command pane (commands/menu.ts). */
  function commandEntries(): CommandEntry[] {
    // A WebUI tab does not offer terminal-only commands (`/to-background`).
    const local = registry.list().filter((spec) => !(store.state.webTab && spec.terminalOnly))
    return mergeCommandEntries(local, store.state.backendCommands, { skillCommands: store.state.structure.commands.skill_commands })
  }

  return {
    ...actions,
    sessionViews,
    openRootSession,
    hostCommand,
    focusProjects,
    openSessionContext,
    openProjectContext,
    /** Register the composer's input (components/Composer.tsx); returns the unregister function. */
    attachComposer(access: ComposerAccess): () => void {
      composer = access
      return () => { if (composer === access) composer = undefined }
    },
    registry,
    submit,
    returnToParent: () => void returnToParent().catch((error: unknown) => status(`Open failed: ${String(error)}`)),
    reviewPending: () => void reviewPending().catch((error: unknown) => status(`Review failed: ${String(error)}`)),
    cancelTurn,
    /** Ctrl+D: quit and leave the session running; in a WebUI tab only a notice (commands/native.ts `toBackground`). */
    toBackground: () => toBackground({ store, client, actions }),
    answer,
    findFiles,
    fileExists,
    /** The composer's chip table (composer/chips.ts). */
    chips,
    pastePaths,
    clipboard,
    clipboardImage,
    /** Pending `@path` image attachments of the composer's current text, for the pending-attachment row (components/Composer.tsx). */
    previewAttachments: (text: string) => loadAttachments(text, true),
    complete: (input: string) => completeCommand(input, { ...store.completionContext(), ...(structure ? { presets: structure.presets() } : {}) }, registry),
    commandEntries,
    modes,
    pickerKey,
    choosePickerRow,
    closePicker,
    /** One key / a paste while the Provider View is open (components/Composer.tsx routes them). */
    providerKey: (key: KeyLike) => providers.key(key),
    providerPaste: (text: string) => providers.paste(text),
    closeProviders: () => providers.close(),
    bundlesKey: (key: KeyLike) => bundles.key(key),
    closeBundles: () => bundles.close(),
    /** One key while the Diff / MCP / Saved Rules / Agents view is open (components/Composer.tsx routes them). */
    diffKey: (key: KeyLike) => diffView.key(key),
    closeDiff: () => diffView.close(),
    mcpKey: (key: KeyLike) => mcp.key(key),
    closeMcp: () => mcp.close(),
    rulesKey: (key: KeyLike) => rules.key(key),
    closeRules: () => rules.close(),
    skillsKey: (key: KeyLike) => skills.key(key),
    closeSkills: () => skills.close(),
    agentsKey: (key: KeyLike) => agentsView.key(key),
    closeAgents: () => agentsView.close(),
    /** One key / a paste while the concealed `/connect-remote` entry is open (components/Composer.tsx routes them). */
    secretKey,
    secretPaste,
    closeSecretEntry: () => { closeSecretEntry(); status("Not connected · /connect-remote cancelled") },
    ui,
    refreshAll,
    start: backend.start,
    close,
    dispose,
  }
}

export { childPollMs } from "./subscriptions"

export type Controller = ReturnType<typeof createController>

/** `newSession` without an active Project on a `--remote` start; the status line already says `noProjectStatus`. */
export class NoProjectError extends Error {
  constructor() {
    super(noProjectStatus)
    this.name = "NoProjectError"
  }
}

/** Hint of the concealed `/connect-remote` entry. */
export const secretEntryHint = "paste or type the relay link (hidden) · Enter connects · Esc cancels"

/** Status shown when an input other than `/connect-remote` holds a relay link: it is never sent. */
export const relayLinkRefusedStatus = "Not sent · the input holds a relay link, which is a secret · /connect-remote takes it"

export { bridgeExitedStatus } from "./backendLifecycle"

/** Status shown when a prompt is submitted while the relay bridge is down. */
export const bridgeDownPromptStatus = "Not sent · the relay bridge exited · /connect-remote <link> connects again"

/** Status shown when a prompt or shell command is submitted while the backend is stopped on purpose. */
export const stoppedPromptStatus = "Not sent · the backend is stopped (hya serve stop) · /reconnect starts it again"

/** Status shown when a prompt is submitted in a subagent's read-only view. */
export { revertReadOnlyStatus as readOnlyStatus } from "./revert"
