/**
 * The controller owns everything asynchronous: catalog refreshes, the session
 * event stream, session creation, prompt/command submission, and the
 * Provider View's calls (app/providers.ts). It writes results into the store; components only read the store and
 * call controller methods.
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
 * subscription are not replayed, so `GET /v1/interactions` is read once
 * after every (re)subscribe and after a `resync`, besides the full catalog
 * refreshes (start, Ctrl+R). `answer()` responds to a prompt
 * (app/prompts.ts).
 *
 * Asks of other sessions: from start, the global stream
 * (`GET /v1/events/stream`, subscribed past every durable seq so only
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
 */
import type { HyaClient, Interaction, MessageInfo, PromptAttachment, SessionInfo, StreamEvent, StreamFrame } from "../client"
import { completeCommand } from "../completion"
import { createCommandRegistry, mergeCommandEntries, openModelPicker, type AppActions, type CommandEntry, type CommandRegistry } from "../commands"
import {
  attachmentName,
  exceedsTurnBudget,
  imageMentionPaths,
  mimeForPath,
  validateAttachmentBytes,
  type AttachmentPreview,
} from "../composer/attachments"
import { findPattern, rankPaths } from "../composer/mention"
import { shellCommand } from "../composer/shell"
import type { KeyLike } from "../keys/bindings"
import { helpPickerHint, helpPickerRows } from "../commands"
import { initialSessionId } from "../launch"
import { askSessionLabel, currentModel, modelReference, otherAskNotice, webNotice } from "../state/format"
import { childActivity, childSessionIds } from "../state/members"
import { editText } from "../composer/editor"
import { savePreferences } from "../prefs"
import { notificationBody, notificationSequence, shouldNotify, type NotifyKind } from "../notify"
import { createPicker, pickerHighlighted, pickerKey as pickerKeyOutcome, type PickerRow, type PickerSpec } from "../state/picker"
import { askFrameRoute, globalAskRoute, type PromptChoice } from "../state/prompts"
import { defaultModelRef } from "../state/providers"
import { sessionRow } from "../state/revert"
import type { AppStore } from "../state/store"
import { createAgentModelsController } from "./agentModels"
import type { UiHandles } from "./context"
import { createDiffController } from "./diff"
import { createMcpController } from "./mcp"
import { createModeSwitcher } from "./modes"
import { createProviderController } from "./providers"
import { answerPrompt } from "./prompts"
import { createRevertController } from "./revert"
import { createRulesController } from "./rules"
import { createDebounce } from "./debounce"
import { createTurnRunner, turnEndStatus } from "./turns"
import { tuiVersion } from "../version"

/** Overlay flush interval: coalesces stream deltas into one render per display frame. */
const flushMs = 16
/** Projection re-read debounce: after 120 ms of quiet, but at least every 400 ms while frames keep coming. */
const refreshWaitMs = 120
const refreshMaxWaitMs = 400
/** Longest wait for the session stream before a prompt is admitted anyway. */
const streamWaitMs = 3000
/** Child-session re-read interval while a child is busy or a turn runs. */
export const childPollMs = 1500
/** Global stream reconnect backoff: the session stream's 800 ms, doubling up to 15 s while it keeps failing (an older backend without the route). */
const globalRetryMs = 800
const globalRetryMaxMs = 15_000

export interface ControllerOptions {
  client: HyaClient
  store: AppStore
  /** Workspace directory for new sessions (`--dir`). */
  directory: string
  registry?: CommandRegistry
  /** Leave the TUI (destroys the renderer, which restores the terminal). */
  quit?: () => void
  /** Which session to open at start (`--continue`, `--session`; src/launch.ts `initialSessionId`). Default: none. */
  startup?: { continue: boolean; session?: string }
  /** Appended to the status line when the backend cannot be reached. */
  connectionHint?: string
  /** TUI preferences file (src/prefs.ts); unset = preference changes apply for this run only. */
  preferencesPath?: string
  /** The terminal behind the renderer (app/run.tsx): clipboard and handing it to an external editor. */
  terminal?: TerminalAccess
  /** Environment for `$VISUAL` / `$EDITOR` (default `process.env`). */
  env?: Record<string, string | undefined>
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

/** Paths requested per `@file` lookup; the best `fileSuggestionLimit` are shown. */
const fileLookupLimit = 50
export const fileSuggestionLimit = 8

export function createController({ client, store, directory, registry = createCommandRegistry(), quit = () => undefined, startup = { continue: false }, connectionHint = "start hya serve", preferencesPath, terminal, env = process.env }: ControllerOptions) {
  let streamAbort: AbortController | undefined
  let globalAbort: AbortController | undefined
  /** Asks a desktop notification was considered for: the open tree's asks arrive on both streams. */
  const notifiedAsks = new Set<string>()
  let flushTimer: ReturnType<typeof setTimeout> | undefined
  let streamReady: Promise<void> = Promise.resolve()
  let closing = false
  /** Projection reads in flight complete out of order; only the newest one is applied. */
  let readsStarted = 0
  let readApplied = 0
  /** Child polling: bumped on session switch so stale reads are dropped. */
  let childGeneration = 0
  let childTimer: ReturnType<typeof setTimeout> | undefined
  let childReading = false
  let childAgain = false
  let lastChildRead = 0
  let unsubscribeFocus: (() => void) | undefined
  const status = (text: string): void => store.setStatus(text)

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
    onEnd: (outcome) => sendNotification(outcome.ok ? "turnFinished" : "turnFailed", outcome.ok ? (store.state.selected?.title ?? "") : outcome.detail),
  })
  const modes = createModeSwitcher({ store, client })

  async function refresh(): Promise<void> {
    const [sessions, interactions, models, agents, workflows, providers, commands, permissionModes] = await Promise.all([
      client.listSessions(), client.listInteractions(), client.listModels(), client.listAgents(), client.listWorkflows(),
      client.listProviders(), client.listCommands(),
      // Optional: the Shift+Tab cycle falls back to the built-ins without it.
      client.listPermissionModes().catch(() => undefined),
    ])
    store.applyCatalog({ sessions, interactions, models, agents, workflows, providers, commands, ...(permissionModes ? { permissionModes } : {}) })
  }

  async function refreshMessages(): Promise<void> {
    const selected = store.state.selected
    if (!selected) return
    const ticket = ++readsStarted
    const rows = await client.listMessages(selected.id)
    // An older read must not replace a newer one: the newer one may already
    // have pruned the overlay text the older snapshot lacks.
    if (ticket < readApplied) return
    readApplied = ticket
    store.setMessages(selected.id, rows)
    trackChildren()
    // A failed turn whose error text was not on the stream: take it from the projection.
    if (store.state.status === "Error · turn failed") {
      const failed = [...store.state.messages].reverse().find((message) => message.error)
      if (failed) status(turnEndStatus({ message: failed.id, role: failed.role, finish: failed.finish }, failed.error))
    }
  }

  /** `GetSessionTodo` for the live sidebar todo panel (E23); silent on failure (an offline backend still shows messages). */
  async function refreshTodos(): Promise<void> {
    const selected = store.state.selected
    if (!selected) return
    store.setTodos(await client.getSessionTodo(selected.id).catch(() => store.state.todos))
  }

  /** `GetVcsStatus` for the status bar's git branch (E22); refreshed on session open and after turns. */
  async function refreshVcs(): Promise<void> {
    const branch = await client.getVcsStatus().then((status) => status.branch ?? "").catch(() => "")
    store.setGitBranch(branch)
  }

  /** Re-read the open session's row: `SessionInfo.usage` after a `tokensRecorded` (E22). */
  async function refreshSession(): Promise<void> {
    const selected = store.state.selected
    if (!selected) return
    const row = await client.request<SessionInfo>("GET", `/v1/sessions/${encodeURIComponent(selected.id)}`)
    const current = store.state.selected
    if (current?.id === row.id) store.setSelected(sessionRow(current, row))
  }

  /** `GET /v1/interactions` into the pending list (after a (re)subscribe or `resync`: frames before it are not replayed). */
  async function refreshInteractions(): Promise<void> {
    store.setInteractions(await client.listInteractions())
  }

  let sessionDue = false
  const refreshLater = createDebounce(() => {
    const session = sessionDue
    sessionDue = false
    // Todos arrive as `todoUpdated` frames and asks as live frames, so only the transcript (and, after billing, the session row) is re-read.
    void Promise.all([refreshMessages(), session ? refreshSession() : undefined])
      .catch((error: unknown) => status(`Refresh failed: ${String(error)}`))
  }, { wait: refreshWaitMs, maxWait: refreshMaxWaitMs })

  function scheduleRefresh(): void {
    refreshLater.schedule()
  }

  /** Re-read only the provider/model catalog (`GET /v1/models` / `GET /v1/providers`): lighter than `refresh()`, so a `catalogUpdated` frame does not also disturb the session list or interactions. */
  async function refreshCatalogOnly(): Promise<void> {
    const [models, providers] = await Promise.all([client.listModels(), client.listProviders()])
    store.setProviderCatalog(providers, models)
  }

  /**
   * `catalogUpdated` (live, no seq, empty `session`) arrives on the global
   * stream and on the open session's own stream, so it is debounced here:
   * one at most every `refreshWaitMs`, coalescing a burst from both streams
   * (or from this client's own Provider View write, which also re-reads the
   * catalog directly) into a single re-read.
   */
  const catalogRefreshLater = createDebounce(() => {
    void refreshCatalogOnly().catch(() => undefined)
  }, { wait: refreshWaitMs, maxWait: refreshMaxWaitMs })

  function scheduleCatalogRefresh(): void {
    catalogRefreshLater.schedule()
  }

  /** Publish the overlay at most once per `flushMs`, however many deltas arrived. */
  function scheduleFlush(): void {
    if (flushTimer) return
    flushTimer = setTimeout(() => {
      flushTimer = undefined
      store.flushOverlay()
    }, flushMs)
  }

  /** Read one child session: its busy flag, agent, latest activity, and whether its newest reply failed. */
  async function readChild(id: string): Promise<void> {
    const [session, messages] = await Promise.all([
      client.request<SessionInfo>("GET", `/v1/sessions/${encodeURIComponent(id)}`),
      client.listMessages(id),
    ])
    const last = [...messages].reverse().find((message: MessageInfo) => message.role === "ROLE_ASSISTANT")
    const activity = childActivity(messages)
    store.setChild(id, {
      busy: session.busy === true,
      agent: session.agent,
      ...(activity ? { activity } : {}),
      ...(last?.error || last?.finish === "FINISH_REASON_ERROR" ? { failed: true } : {}),
    })
  }

  /**
   * Re-read the open session's child sessions: at once when the last round
   * is older than `childPollMs`, else when it is due (at most one round per
   * `childPollMs`). Rounds repeat while a child is busy or this client's
   * turn runs.
   */
  function trackChildren(): void {
    if (childReading) {
      childAgain = true
      return
    }
    if (childTimer || closing) return
    childTimer = setTimeout(readChildren, Math.max(0, lastChildRead + childPollMs - Date.now()))
  }

  function readChildren(): void {
    childTimer = undefined
    const ids = childSessionIds(store.state.members, store.state.messages)
    if (!ids.length) return
    const generation = childGeneration
    childReading = true
    childAgain = false
    lastChildRead = Date.now()
    void Promise.all([
      ...ids.map((id) => readChild(id).catch(() => undefined)),
      client.listSessions().then((rows) => generation === childGeneration && store.setSessions(rows)).catch(() => undefined),
    ]).then(() => {
      if (generation !== childGeneration) return
      childReading = false
      const busy = ids.some((id) => store.state.children.get(id)?.busy)
      if (childAgain || busy || store.state.running) trackChildren()
    })
  }

  function applyEvent(event: StreamEvent): void {
    // Live, no seq, empty `session`: the provider/model catalog changed. Not
    // a transcript or ask frame, so it never reaches the store's fold —
    // debounced here instead of `scheduleRefresh()`'s projection re-read.
    if (event.catalogUpdated) {
      scheduleCatalogRefresh()
      return
    }
    const effect = store.applyEvent(event)
    if (event.memberUpdated && effect.durable) trackChildren()
    if (effect.changed) scheduleFlush()
    // Text deltas only feed the overlay. Other durable frames (message and
    // part boundaries, tool state, errors) and live interaction frames
    // change the projection or the pending list: re-read them.
    const delta = event.partAppended || event.partReplaced || (!effect.durable && (event.partStarted || event.partCompleted))
    // Ask frames change only the pending list, which the store already updated.
    const ask = event.permissionRequested || event.questionRequested || event.interactionResolved
    // A fresh ask of the open session's own turn (not a descendant's, which never reaches applyEvent).
    const asked = event.permissionRequested?.interaction ?? event.questionRequested?.interaction
    if (asked) notifyAsk(asked.id, event.permissionRequested ? "permission" : "question", asked.title ?? "")
    if (event.tokensRecorded && effect.durable) sessionDue = true
    // A revert or redo (maybe another client's): re-read the session row (`revert`) with the transcript.
    if (event.sessionReverted && effect.durable) sessionDue = true
    if (!delta && !ask && (effect.durable || !event.seq)) scheduleRefresh()
    // A turn ended: the working directory's git status may have changed (E22),
    // and the sidebar's session-list row (possibly stale from before this
    // session was opened, or from another client's turn) is no longer busy.
    if (effect.finished) {
      void refreshVcs()
      const selected = store.state.selected
      if (selected) store.setSessionBusy(selected.id, false)
    }
    turns.observe(effect)
  }

  /** Replay durable events after the last applied seq (ListEvents), then drop what the projection shows finished. */
  async function gapFill(sessionId: string): Promise<void> {
    const events = await client.listEventsSince(sessionId, store.fold.lastSeq)
    if (store.state.selected?.id !== sessionId) return
    for (const event of events) applyEvent(event)
    store.setMessages(sessionId, store.state.messages)
  }

  async function onFrame(frame: StreamFrame, sessionId: string): Promise<void> {
    if (store.state.selected?.id !== sessionId) return
    if (frame.resync) {
      store.markLiveLost()
      await gapFill(sessionId)
      // Live ask frames in the gap are lost too.
      await refreshInteractions().catch(() => undefined)
      scheduleRefresh()
      return
    }
    const event = frame.event
    if (!event) return
    const route = askFrameRoute(event, sessionId)
    if (route === "own") applyEvent(event)
    else if (route === "descendantAsk") store.applyAsk(event)
  }

  function startStream(sessionId: string): void {
    streamAbort?.abort()
    const controller = new AbortController()
    streamAbort = controller
    let connections = 0
    let ready!: () => void
    streamReady = new Promise((resolve) => (ready = resolve))
    void (async () => {
      while (!closing && !controller.signal.aborted) {
        try {
          await client.streamSession(sessionId, store.fold.lastSeq, (frame) => onFrame(frame, sessionId), controller.signal, async () => {
            // Subscribed: frames after this point are buffered by the
            // connection while the gap since the last applied seq is filled.
            await gapFill(sessionId)
            // Asks raised before this subscription are not replayed: list them once.
            await refreshInteractions().catch(() => undefined)
            store.setConnected(true)
            if (connections++ > 0) scheduleRefresh()
            ready()
          }, true)
        } catch (error) {
          if (!controller.signal.aborted) {
            store.setConnected(false)
            status(`Stream reconnecting: ${String(error)}`)
          }
        }
        ready()
        if (!controller.signal.aborted) await Bun.sleep(800)
      }
    })()
  }

  /**
   * One frame of the global stream: asks and resolves only. The open tree's
   * are also on its session stream (applied there; kept by id here too). An
   * ask of another session is shown in the pending block with its session,
   * announced on the status line, and notified while unfocused.
   */
  async function onGlobalFrame(frame: StreamFrame): Promise<void> {
    if (frame.resync) {
      // Ask frames in the gap are lost: list the pending asks again.
      await refreshInteractions().catch(() => undefined)
      return
    }
    const event = frame.event
    if (!event) return
    // Live, no seq, empty `session`: never an ask, so `globalAskRoute` would
    // ignore it; debounced with the copy that may also arrive on the open
    // session's own stream (`applyEvent`), so a burst on both is one re-read.
    if (event.catalogUpdated) {
      scheduleCatalogRefresh()
      return
    }
    const route = globalAskRoute(event, store.state)
    if (route === "ignore") return
    const raw = event.permissionRequested?.interaction ?? event.questionRequested?.interaction
    const asked = raw && { ...raw, session: raw.session || event.session }
    const fresh = asked !== undefined && !store.state.interactions.some((row) => row.id === asked.id)
    store.applyAsk(event)
    if (route !== "other" || !asked || !fresh || !store.state.interactions.some((row) => row.id === asked.id)) return
    // A session created since the last listing (another client's, a headless run): list it, so the ask can name it.
    if (asked.session && !store.state.sessions.some((row) => row.id === asked.session)) {
      await client.listSessions().then((rows) => store.setSessions(rows)).catch(() => undefined)
    }
    status(otherAskNotice(asked, store.state.sessions))
    const where = asked.session ? ` · in ${askSessionLabel(asked.session, store.state.sessions)}` : ""
    notifyAsk(asked.id, event.permissionRequested ? "permission" : "question", `${asked.title ?? ""}${where}`)
  }

  /** Subscribe to the global stream for the life of the TUI; reconnect with a backoff. */
  function startGlobalStream(): void {
    globalAbort?.abort()
    const controller = new AbortController()
    globalAbort = controller
    let delay = globalRetryMs
    void (async () => {
      while (!closing && !controller.signal.aborted) {
        try {
          await client.streamGlobal((frame) => onGlobalFrame(frame), controller.signal, async () => {
            delay = globalRetryMs
            // Asks raised before this subscription are not replayed: list them once.
            await refreshInteractions().catch(() => undefined)
          })
        } catch {
          // Silent: the session stream owns the connection state; this one only adds other sessions' asks.
        }
        if (controller.signal.aborted) break
        await new Promise<void>((resolve) => {
          const timer = setTimeout(resolve, delay)
          controller.signal.addEventListener("abort", () => { clearTimeout(timer); resolve() }, { once: true })
        })
        delay = Math.min(delay * 2, globalRetryMaxMs)
      }
    })()
  }

  async function openSession(sessionId: string): Promise<void> {
    const listed = store.state.sessions.find((row) => row.id === sessionId)
    // A fresh read gives the current `lastSeq`, so the stream gap-fill stays small.
    const session = await client.request<SessionInfo>("GET", `/v1/sessions/${encodeURIComponent(sessionId)}`)
      .catch((error: unknown) => {
        if (listed) return listed
        throw error
      })
    childGeneration++
    if (childTimer) clearTimeout(childTimer)
    childTimer = undefined
    childReading = false
    childAgain = false
    lastChildRead = 0
    store.openSession(session)
    await refreshMessages()
    void refreshTodos()
    void refreshVcs()
    startStream(session.id)
  }

  /** Leave a subagent's read-only view: open its parent session. */
  async function returnToParent(): Promise<void> {
    const parent = store.state.selected?.parent
    if (!parent) return
    await openSession(parent)
    status("Back to the parent session")
  }

  async function newSession(agentArg?: string, modelArg?: string): Promise<void> {
    const { agents } = store.state
    // A `/model`/`/agent` choice made before any session existed (state/picker.ts, C11/C12) applies to
    // the next `CreateSession` the same way an explicit argument would.
    const agent = agentArg ?? store.state.pendingAgent ?? agents.find((item) => !item.hidden)?.name ?? "build"
    const model = modelArg ?? (defaultModelRef({ ...store.state, selected: undefined, pendingAgent: agent }) || undefined)
    if (!model) throw new Error("No model is available; configure a provider on the backend")
    const session = await client.createSession(agent, model, directory)
    store.setPendingAgent(undefined)
    store.setPendingModel(undefined)
    await refresh()
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
   * `/editor`, Ctrl+X Ctrl+E: the input in the external editor
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
  const rules = createRulesController({ store, client })
  const agentModels = createAgentModelsController({ store, client, openPicker })
  const revert = createRevertController({ store, client, composer: () => composer, openSession, refresh, openPicker })

  const actions: AppActions = {
    refresh, refreshMessages, openSession, newSession, scheduleRefresh, openHelp, openEditor,
    openProviders: () => providers.open(),
    openDiff: () => diffView.open(),
    openMcp: () => mcp.open(),
    openRules: () => rules.open(),
    openAgentModels: () => agentModels.open(),
    copyText: (text) => terminal?.copy(text) ?? false,
    undo: () => revert.undo(),
    redo: () => revert.redo(),
    fork: () => revert.fork(),
    cancelTurn: () => turns.cancel(),
    quit,
    openPicker,
    requestPermissionMode: (mode) => modes.request(mode),
    savePreferences: (patch) => { if (preferencesPath) savePreferences(preferencesPath, patch) },
  }

  /**
   * Resolve every `@path` image mention in `text` to a `PromptAttachment`
   * candidate: read the file (relative to the open session's workdir, else
   * `--dir`), and check the per-file and per-turn size caps
   * (composer/attachments.ts, docs/protocol/README.md "Prompt attachments
   * (images)"). Never throws: a file that cannot be read or fails validation
   * gets `error` set instead of `data`, so the caller can show it without
   * sending. Also used by the composer to preview pending attachments as the
   * user types (components/Composer.tsx).
   */
  async function loadAttachments(text: string): Promise<AttachmentPreview[]> {
    const paths = imageMentionPaths(text)
    if (!paths.length) return []
    const workdir = (store.state.selected?.workdir || directory).replace(/\/+$/, "")
    const sizes: number[] = []
    const previews: AttachmentPreview[] = []
    for (const path of paths) {
      const name = attachmentName(path)
      const absolute = path.startsWith("/") ? path : `${workdir}/${path}`
      try {
        const file = Bun.file(absolute)
        if (!(await file.exists())) {
          previews.push({ path, name, error: "file not found" })
          continue
        }
        const bytes = new Uint8Array(await file.arrayBuffer())
        const sizeError = validateAttachmentBytes(path, bytes.byteLength)
        if (sizeError) {
          previews.push({ path, name, size: bytes.byteLength, error: sizeError })
          continue
        }
        if (exceedsTurnBudget(sizes, bytes.byteLength)) {
          previews.push({ path, name, size: bytes.byteLength, error: "attachments over 20 MiB for this turn" })
          continue
        }
        sizes.push(bytes.byteLength)
        previews.push({ path, name, mime: mimeForPath(path), size: bytes.byteLength, data: Buffer.from(bytes).toString("base64") })
      } catch (error) {
        previews.push({ path, name, error: String(error) })
      }
    }
    return previews
  }

  /** Submit one composer input: a prompt, a `!command` shell turn, a native command, or a backend command. */
  async function submit(value: string): Promise<void> {
    const text = value.trim()
    if (!text) return
    try {
      if (text.startsWith("/")) {
        await registry.dispatch(text, { store, client, actions })
        return
      }
      if (store.state.selected?.parent) {
        status(readOnlyStatus)
        return
      }
      const command = shellCommand(text)
      if (command === "") throw new Error("Usage: !<shell command>")
      if (!store.state.selected) await newSession()
      if (command !== undefined) {
        store.followTranscript()
        await Promise.race([streamReady, Bun.sleep(streamWaitMs)])
        await turns.submit(command, { shell: true })
        return
      }
      const previews = await loadAttachments(text)
      const failed = previews.filter((item) => item.error)
      if (failed.length) {
        status(`Not sent · ${failed.map((item) => `${item.name}: ${item.error}`).join(" · ")}`)
        return
      }
      if (previews.length && currentModel(store.state)?.imageInput === false) {
        status(`Not sent · ${modelReference(store.state.selected!)} does not accept image attachments`)
        return
      }
      const attachments: PromptAttachment[] = previews.map((item) => ({ name: item.name, mime: item.mime, data: item.data ?? "", path: item.path }))
      store.followTranscript()
      // Subscribe before CreateTurn, so no frame of the new turn is missed.
      await Promise.race([streamReady, Bun.sleep(streamWaitMs)])
      await turns.submit(text, attachments.length ? { attachments } : {})
    } catch (error) {
      status(`Error: ${String(error)}`)
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

  /** Whether `path` (relative to the open session's workdir, else `--dir`) exists — a pasted path becomes an `@path ` mention only when it does (components/Composer.tsx). */
  async function fileExists(path: string): Promise<boolean> {
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
   * Initial load: bootstrap, catalogs, then the session `startup` names
   * (`--session <id>`, or `--continue`: the most recent top-level session of
   * `--dir`); without either no session is open until the first prompt or
   * `/new` creates one.
   */
  async function start(): Promise<void> {
    unsubscribeFocus = terminal?.onFocusChange?.((focused) => store.setFocused(focused))
    try {
      const bootstrap = await client.bootstrap()
      store.applyBootstrap(bootstrap)
      await refresh()
      void refreshVcs()
      startGlobalStream()
      const target = initialSessionId(store.state.sessions, startup, directory)
      let missing = ""
      if (target) await openSession(target).catch(() => { missing = ` · session ${target} not found` })
      else if (startup.continue) missing = " · no earlier session in this directory"
      const version = bootstrap.location?.version ?? ""
      const mismatch = version && version !== tuiVersion ? ` · backend ${version} ≠ tui ${tuiVersion}` : ""
      // A WebUI that bare `hya` could not start is the one notice worth the status line.
      status(webNotice(store.state.web) ?? `Connected to hya ${version} · ? or /help for keys and commands${missing}${mismatch}`)
    } catch (error) {
      status(`Connection failed: ${String(error)} · ${connectionHint}`)
      store.setView("help")
      store.markReady()
    }
  }

  function dispose(): void {
    closing = true
    streamAbort?.abort()
    globalAbort?.abort()
    if (childTimer) clearTimeout(childTimer)
    refreshLater.cancel()
    if (flushTimer) clearTimeout(flushTimer)
    providers.dispose()
    diffView.dispose()
    mcp.dispose()
    rules.dispose()
    agentModels.dispose()
    unsubscribeFocus?.()
  }

  /** Merged, deduplicated command list for the `/` command menu (commands/menu.ts). */
  function commandEntries(): CommandEntry[] {
    return mergeCommandEntries(registry.list(), store.state.backendCommands)
  }

  return {
    ...actions,
    /** Register the composer's input (components/Composer.tsx); returns the unregister function. */
    attachComposer(access: ComposerAccess): () => void {
      composer = access
      return () => { if (composer === access) composer = undefined }
    },
    registry,
    submit,
    returnToParent: () => void returnToParent().catch((error: unknown) => status(`Open failed: ${String(error)}`)),
    cancelTurn,
    answer,
    findFiles,
    fileExists,
    /** Pending `@path` image attachments of the composer's current text, for the pending-attachment row (components/Composer.tsx). */
    previewAttachments: loadAttachments,
    complete: (input: string) => completeCommand(input, store.completionContext(), registry),
    commandEntries,
    modes,
    pickerKey,
    choosePickerRow,
    closePicker,
    /** One key / a paste while the Provider View is open (components/Composer.tsx routes them). */
    providerKey: (key: KeyLike) => providers.key(key),
    providerPaste: (text: string) => providers.paste(text),
    closeProviders: () => providers.close(),
    /** One key while the Diff / MCP / Saved Rules / Agent Models view is open (components/Composer.tsx routes them). */
    diffKey: (key: KeyLike) => diffView.key(key),
    closeDiff: () => diffView.close(),
    mcpKey: (key: KeyLike) => mcp.key(key),
    closeMcp: () => mcp.close(),
    rulesKey: (key: KeyLike) => rules.key(key),
    closeRules: () => rules.close(),
    agentModelsKey: (key: KeyLike) => agentModels.key(key),
    closeAgentModels: () => agentModels.close(),
    /** Shared with the AppContext `ui` prop (app/run.tsx): the Diff view registers its scroller here. */
    ui,
    refreshAll,
    start,
    dispose,
  }
}

export type Controller = ReturnType<typeof createController>

/** Status shown when a prompt is submitted in a subagent's read-only view. */
export const readOnlyStatus = "Read-only: this is a subagent's session · Esc returns to the parent"
