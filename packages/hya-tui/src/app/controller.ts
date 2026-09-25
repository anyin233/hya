/**
 * The controller owns everything asynchronous: catalog refreshes, the session
 * event stream, session creation, prompt/command submission, and concealed key
 * entry. It writes results into the store; components only read the store and
 * call controller methods.
 *
 * The TUI reads the server projection (sessions, transcript, interactions);
 * stream frames only trigger a debounced re-read. A transient streaming
 * overlay belongs next to `onFrame`, never in place of the projection.
 */
import type { HyaClient, SessionInfo, StreamFrame } from "../client"
import { completeCommand, SecretEntry } from "../completion"
import { createCommandRegistry, type AppActions, type CommandRegistry } from "../commands"
import type { KeyLike } from "../keys/bindings"
import type { AppStore } from "../state/store"

export interface ControllerOptions {
  client: HyaClient
  store: AppStore
  /** Workspace directory for new sessions (`--dir`). */
  directory: string
  registry?: CommandRegistry
}

export function createController({ client, store, directory, registry = createCommandRegistry() }: ControllerOptions) {
  let streamAbort: AbortController | undefined
  let refreshTimer: ReturnType<typeof setTimeout> | undefined
  let closing = false
  const secret = new SecretEntry()
  const status = (text: string): void => store.setStatus(text)

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
    store.setMessages(selected.id, await client.listMessages(selected.id))
  }

  function scheduleRefresh(): void {
    if (refreshTimer) clearTimeout(refreshTimer)
    refreshTimer = setTimeout(() => {
      void Promise.all([refreshMessages(), client.listInteractions().then((rows) => store.setInteractions(rows))])
        .catch((error: unknown) => status(`Refresh failed: ${String(error)}`))
    }, 120)
  }

  async function onFrame(frame: StreamFrame): Promise<void> {
    const selected = store.state.selected
    if (frame.resync && selected) {
      const replay = await client.listEvents(selected.id, store.state.cursor)
      store.setCursor(replay.nextSeq ?? store.state.cursor)
      scheduleRefresh()
      return
    }
    const event = frame.event
    if (!event) return
    if (event.seq) store.advanceCursor(event.seq)
    if (event.messageFinished && store.finishTurn(event.messageFinished.message)) {
      status(`Turn finished · ${event.messageFinished.finish ?? "done"}`)
    }
    scheduleRefresh()
  }

  function startStream(sessionId: string): void {
    streamAbort?.abort()
    const controller = new AbortController()
    streamAbort = controller
    void (async () => {
      while (!closing && !controller.signal.aborted) {
        try {
          await client.streamSession(sessionId, store.state.cursor, onFrame, controller.signal)
        } catch (error) {
          if (!controller.signal.aborted) status(`Stream reconnecting: ${String(error)}`)
        }
        if (!controller.signal.aborted) await Bun.sleep(800)
      }
    })()
  }

  async function openSession(sessionId: string): Promise<void> {
    const session = store.state.sessions.find((row) => row.id === sessionId)
      ?? await client.request<SessionInfo>("GET", `/v1/sessions/${encodeURIComponent(sessionId)}`)
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

  const actions: AppActions = { refresh, refreshMessages, openSession, newSession, beginKeyEntry, scheduleRefresh }

  /** Submit one composer line: a prompt, a native command, or a backend command. */
  async function submit(value: string): Promise<void> {
    const text = value.trim()
    if (!text) return
    try {
      if (text.startsWith("/")) {
        await registry.dispatch(text, { store, client, actions })
        return
      }
      if (!store.state.selected) await newSession()
      const selected = store.state.selected
      if (!selected) throw new Error("Session creation failed")
      const turn = await client.createTurn(selected.id, text)
      store.setTurn(turn.id)
      status(`Turn ${turn.state.toLowerCase()} · ${turn.id}`)
      scheduleRefresh()
    } catch (error) {
      status(`Error: ${String(error)}`)
    }
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
    if (refreshTimer) clearTimeout(refreshTimer)
    secret.clear()
  }

  return {
    ...actions,
    registry,
    submit,
    complete: (input: string) => completeCommand(input, store.completionContext(), registry),
    secretKey,
    secretPaste,
    refreshAll,
    start,
    dispose,
  }
}

export type Controller = ReturnType<typeof createController>
