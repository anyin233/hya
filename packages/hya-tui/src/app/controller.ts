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
 */
import type { HyaClient, SessionInfo, StreamEvent, StreamFrame } from "../client"
import { completeCommand, SecretEntry } from "../completion"
import { createCommandRegistry, mergeCommandEntries, type AppActions, type CommandEntry, type CommandRegistry } from "../commands"
import { findPattern, rankPaths } from "../composer/mention"
import { shellCommand } from "../composer/shell"
import type { KeyLike } from "../keys/bindings"
import type { AppStore } from "../state/store"
import { createDebounce } from "./debounce"
import { createTurnRunner, turnEndStatus } from "./turns"

/** Overlay flush interval: coalesces stream deltas into one render per display frame. */
const flushMs = 16
/** Projection re-read debounce: after 120 ms of quiet, but at least every 400 ms while frames keep coming. */
const refreshWaitMs = 120
const refreshMaxWaitMs = 400
/** Longest wait for the session stream before a prompt is admitted anyway. */
const streamWaitMs = 3000

export interface ControllerOptions {
  client: HyaClient
  store: AppStore
  /** Workspace directory for new sessions (`--dir`). */
  directory: string
  registry?: CommandRegistry
  /** Leave the TUI (destroys the renderer, which restores the terminal). */
  quit?: () => void
}

/** Paths requested per `@file` lookup; the best `fileSuggestionLimit` are shown. */
const fileLookupLimit = 50
export const fileSuggestionLimit = 8

export function createController({ client, store, directory, registry = createCommandRegistry(), quit = () => undefined }: ControllerOptions) {
  let streamAbort: AbortController | undefined
  let flushTimer: ReturnType<typeof setTimeout> | undefined
  let streamReady: Promise<void> = Promise.resolve()
  let closing = false
  /** Projection reads in flight complete out of order; only the newest one is applied. */
  let readsStarted = 0
  let readApplied = 0
  const secret = new SecretEntry()
  const status = (text: string): void => store.setStatus(text)
  const turns = createTurnRunner({ store, client })

  async function refresh(): Promise<void> {
    const [sessions, interactions, models, workflows, providers, savedKeys, commands] = await Promise.all([
      client.listSessions(), client.listInteractions(), client.listModels(), client.listWorkflows(),
      client.listProviders(), client.listSavedKeys(), client.listCommands(),
    ])
    store.applyCatalog({ sessions, interactions, models, workflows, providers, savedKeys, commands })
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
    // A failed turn whose error text was not on the stream: take it from the projection.
    if (store.state.status === "Error · turn failed") {
      const failed = [...store.state.messages].reverse().find((message) => message.error)
      if (failed) status(turnEndStatus({ message: failed.id, role: failed.role, finish: failed.finish }, failed.error))
    }
  }

  const refreshLater = createDebounce(() => {
    void Promise.all([refreshMessages(), client.listInteractions().then((rows) => store.setInteractions(rows))])
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

  function applyEvent(event: StreamEvent): void {
    const effect = store.applyEvent(event)
    if (effect.changed) scheduleFlush()
    // Text deltas only feed the overlay. Other durable frames (message and
    // part boundaries, tool state, errors) and live interaction frames
    // change the projection or the pending list: re-read them.
    const delta = event.partAppended || event.partReplaced || (!effect.durable && (event.partStarted || event.partCompleted))
    if (!delta && (effect.durable || !event.seq)) scheduleRefresh()
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
      scheduleRefresh()
      return
    }
    const event = frame.event
    if (!event || (event.session && event.session !== sessionId)) return
    applyEvent(event)
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
            if (connections++ > 0) scheduleRefresh()
            ready()
          })
        } catch (error) {
          if (!controller.signal.aborted) status(`Stream reconnecting: ${String(error)}`)
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
    store.openSession(session)
    await refreshMessages()
    startStream(session.id)
  }

  async function newSession(agentArg?: string, modelArg?: string): Promise<void> {
    const { agents, models } = store.state
    const agent = agentArg ?? agents.find((item) => !item.hidden)?.name ?? "build"
    const preferred = agents.find((item) => item.name === agent)?.model
    const model = modelArg ?? (preferred?.providerId && preferred.modelId ? `${preferred.providerId}/${preferred.modelId}` : models[0]?.id)
    if (!model) throw new Error("No model is available; configure a provider on the backend")
    const session = await client.createSession(agent, model, directory)
    await refresh()
    await openSession(session.id)
    status(`Created ${session.id}`)
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

  const actions: AppActions = {
    refresh, refreshMessages, openSession, newSession, beginKeyEntry, scheduleRefresh,
    cancelTurn: () => turns.cancel(),
    quit,
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

  /** Initial load: bootstrap, catalogs, open the newest session. */
  async function start(): Promise<void> {
    try {
      const bootstrap = await client.bootstrap()
      store.applyBootstrap(bootstrap)
      await refresh()
      const first = store.state.sessions[0]
      if (first) await openSession(first.id)
      const version = bootstrap.location?.version ?? ""
      status(store.state.savedKeysAvailable
        ? `Connected to hya ${version} · /help for commands`
        : `Connected to hya ${version} · key listing needs backend 0.41.0+`)
    } catch (error) {
      status(`Connection failed: ${String(error)} · start hya serve`)
      store.setView("help")
      store.markReady()
    }
  }

  function dispose(): void {
    closing = true
    streamAbort?.abort()
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
    cancelTurn,
    findFiles,
    complete: (input: string) => completeCommand(input, store.completionContext(), registry),
    commandEntries,
    secretKey,
    secretPaste,
    refreshAll,
    start,
    dispose,
  }
}

export type Controller = ReturnType<typeof createController>
