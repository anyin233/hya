/**
 * The controller owns everything asynchronous: catalog refreshes, the session
 * event stream, session creation, prompt/command submission, and concealed key
 * entry. It writes results into the store; components only read the store and
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
 * Usage and todos: `tokensRecorded`, `todoUpdated`, and `compactionApplied`
 * fold in the store; a `tokensRecorded` also re-reads the open session
 * (debounced) for its authoritative `SessionInfo.usage` total.
 */
import type { HyaClient, Interaction, MessageInfo, SessionInfo, StreamEvent, StreamFrame } from "../client"
import { completeCommand, SecretEntry } from "../completion"
import { createCommandRegistry, mergeCommandEntries, type AppActions, type CommandEntry, type CommandRegistry } from "../commands"
import { findPattern, rankPaths } from "../composer/mention"
import { shellCommand } from "../composer/shell"
import type { KeyLike } from "../keys/bindings"
import { helpPickerHint, helpPickerRows } from "../commands"
import { initialSessionId } from "../launch"
import { webNotice } from "../state/format"
import { childActivity, childSessionIds } from "../state/members"
import { savePreferences } from "../prefs"
import { createPicker, pickerHighlighted, pickerKey as pickerKeyOutcome, type PickerRow, type PickerSpec } from "../state/picker"
import { askFrameRoute, type PromptChoice } from "../state/prompts"
import type { AppStore } from "../state/store"
import { createModeSwitcher } from "./modes"
import { answerPrompt } from "./prompts"
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
}

/** Rows the help overlay shows at once (bounded by the terminal height, components/Picker.tsx). */
const helpMaxRows = 40

/** Paths requested per `@file` lookup; the best `fileSuggestionLimit` are shown. */
const fileLookupLimit = 50
export const fileSuggestionLimit = 8

export function createController({ client, store, directory, registry = createCommandRegistry(), quit = () => undefined, startup = { continue: false }, connectionHint = "start hya serve", preferencesPath }: ControllerOptions) {
  let streamAbort: AbortController | undefined
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
  const secret = new SecretEntry()
  const status = (text: string): void => store.setStatus(text)
  const turns = createTurnRunner({ store, client })
  const modes = createModeSwitcher({ store, client })

  async function refresh(): Promise<void> {
    const [sessions, interactions, models, agents, workflows, providers, savedKeys, commands, permissionModes] = await Promise.all([
      client.listSessions(), client.listInteractions(), client.listModels(), client.listAgents(), client.listWorkflows(),
      client.listProviders(), client.listSavedKeys(), client.listCommands(),
      // Optional: the Shift+Tab cycle falls back to the built-ins without it.
      client.listPermissionModes().catch(() => undefined),
    ])
    store.applyCatalog({ sessions, interactions, models, agents, workflows, providers, savedKeys, commands, ...(permissionModes ? { permissionModes } : {}) })
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
    if (current?.id === row.id) store.setSelected({ ...current, ...row })
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
    const effect = store.applyEvent(event)
    if (event.memberUpdated && effect.durable) trackChildren()
    if (effect.changed) scheduleFlush()
    // Text deltas only feed the overlay. Other durable frames (message and
    // part boundaries, tool state, errors) and live interaction frames
    // change the projection or the pending list: re-read them.
    const delta = event.partAppended || event.partReplaced || (!effect.durable && (event.partStarted || event.partCompleted))
    // Ask frames change only the pending list, which the store already updated.
    const ask = event.permissionRequested || event.questionRequested || event.interactionResolved
    if (event.tokensRecorded && effect.durable) sessionDue = true
    if (!delta && !ask && (effect.durable || !event.seq)) scheduleRefresh()
    // A turn ended: the working directory's git status may have changed (E22).
    if (effect.finished) void refreshVcs()
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
    const { agents, models } = store.state
    // A `/model`/`/agent` choice made before any session existed (state/picker.ts, C11/C12) applies to
    // the next `CreateSession` the same way an explicit argument would.
    const agent = agentArg ?? store.state.pendingAgent ?? agents.find((item) => !item.hidden)?.name ?? "build"
    const preferred = agents.find((item) => item.name === agent)?.model
    const model = modelArg ?? store.state.pendingModel ?? (preferred?.providerId && preferred.modelId ? `${preferred.providerId}/${preferred.modelId}` : models[0]?.id)
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

  function beginKeyEntry(provider: string): void {
    secret.clear()
    store.beginSecret(provider)
    status(`Enter API key for ${provider} · Enter saves · Esc cancels`)
  }

  function finishKeyEntry(): void {
    secret.clear()
    store.endSecret()
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

  const actions: AppActions = {
    refresh, refreshMessages, openSession, newSession, beginKeyEntry, scheduleRefresh, openHelp,
    cancelTurn: () => turns.cancel(),
    quit,
    openPicker,
    requestPermissionMode: (mode) => modes.request(mode),
    savePreferences: (patch) => { if (preferencesPath) savePreferences(preferencesPath, patch) },
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
      store.followTranscript()
      // Subscribe before CreateTurn, so no frame of the new turn is missed.
      await Promise.race([streamReady, Bun.sleep(streamWaitMs)])
      if (command !== undefined) await turns.submit(command, { shell: true })
      else await turns.submit(text)
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

  /** Handle one key during concealed key entry. The key never reaches the store. */
  function secretKey(key: KeyLike): void {
    const provider = store.state.secretProvider
    if (!provider) return
    if (key.name === "escape" || key.name === "esc") {
      finishKeyEntry()
      status("Key entry cancelled")
    } else if (key.name === "backspace") {
      secret.backspace()
      store.setSecretMask(secret.mask)
    } else if (key.name === "return" || key.name === "enter" || key.sequence === "\r") {
      const value = secret.take()
      if (!value) {
        status("API key cannot be empty · Esc cancels")
        return
      }
      finishKeyEntry()
      void client.setProviderKey(provider, value)
        .then(async () => {
          store.setView("keys")
          await refresh()
          status(`Saved key for ${provider} · restart backend to apply`)
        })
        .catch((error: unknown) => status(`Key save failed: ${String(error)}`))
    } else if (!key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " ") {
      secret.append(key.sequence)
      store.setSecretMask(secret.mask)
    }
  }

  function secretPaste(text: string): void {
    secret.append(text)
    store.setSecretMask(secret.mask)
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
    try {
      const bootstrap = await client.bootstrap()
      store.applyBootstrap(bootstrap)
      await refresh()
      void refreshVcs()
      const target = initialSessionId(store.state.sessions, startup, directory)
      let missing = ""
      if (target) await openSession(target).catch(() => { missing = ` · session ${target} not found` })
      else if (startup.continue) missing = " · no earlier session in this directory"
      const version = bootstrap.location?.version ?? ""
      const mismatch = version && version !== tuiVersion ? ` · backend ${version} ≠ tui ${tuiVersion}` : ""
      // A WebUI that bare `hya` could not start is the one notice worth the status line.
      status(webNotice(store.state.web) ?? (store.state.savedKeysAvailable
        ? `Connected to hya ${version} · ? or /help for keys and commands${missing}${mismatch}`
        : `Connected to hya ${version} · key listing needs backend 0.41.0+${missing}${mismatch}`))
    } catch (error) {
      status(`Connection failed: ${String(error)} · ${connectionHint}`)
      store.setView("help")
      store.markReady()
    }
  }

  function dispose(): void {
    closing = true
    streamAbort?.abort()
    if (childTimer) clearTimeout(childTimer)
    refreshLater.cancel()
    if (flushTimer) clearTimeout(flushTimer)
    secret.clear()
  }

  /** Merged, deduplicated command list for the `/` command menu (commands/menu.ts). */
  function commandEntries(): CommandEntry[] {
    return mergeCommandEntries(registry.list(), store.state.backendCommands)
  }

  return {
    ...actions,
    registry,
    submit,
    returnToParent: () => void returnToParent().catch((error: unknown) => status(`Open failed: ${String(error)}`)),
    cancelTurn,
    answer,
    findFiles,
    complete: (input: string) => completeCommand(input, store.completionContext(), registry),
    commandEntries,
    modes,
    pickerKey,
    choosePickerRow,
    closePicker,
    secretKey,
    secretPaste,
    refreshAll,
    start,
    dispose,
  }
}

export type Controller = ReturnType<typeof createController>

/** Status shown when a prompt is submitted in a subagent's read-only view. */
export const readOnlyStatus = "Read-only: this is a subagent's session · Esc returns to the parent"
