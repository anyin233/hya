import type { Bridge, BridgeFlags } from "../bridge"
import { HttpError, type HyaClient, type ProjectInfo, type SessionInfo, type StreamEvent } from "../client"
import { initialSessionId, probeHealth } from "../launch"
import { stripTerminalControls } from "../sanitize"
import { webNotice } from "../state/format"
import { newestTopLevelSession, noProjectStatus } from "../state/projects"
import { treeSessionIds } from "../state/prompts"
import type { AppStore } from "../state/store"
import { backendVersionError } from "../version"
import { createReconnector, type ServerSwitch } from "./reconnect"

interface BackendOptions {
  client: HyaClient
  store: AppStore
  directory: string
  startedRemote: boolean
  startup: { continue: boolean; session?: string; resume?: { id?: string } }
  connectionHint: string
  reconnect: (() => Promise<ServerSwitch>) | undefined
  find: (() => Promise<ServerSwitch | undefined>) | undefined
  onRestarted: (() => void) | undefined
  probe: ((url: string) => Promise<boolean>) | undefined
  startRemoteBridge: ((link: string, flags: BridgeFlags, onLine: (line: string) => void) => Promise<Bridge>) | undefined
  home: (() => Promise<ServerSwitch>) | undefined
  onScopeChanged: (() => void | Promise<void>) | undefined
  status: (text: string) => void
  isClosing: () => boolean
  refresh: () => Promise<void>
  refreshVcs: () => Promise<void>
  subscribeFocus: () => void
  startGlobalStream: () => void
  startStream: (sessionId: string) => void
  stopGlobalStream: () => void
  leaveOpenSession: () => Promise<void>
  activateProject: (project: ProjectInfo) => void
  openProjectOverlay: () => void
  openSession: (sessionId: string) => Promise<void>
  newSession: () => Promise<void>
  resume: (id: string | undefined, onCancel?: () => void) => Promise<void>
}

/** How long the startup list waits for MCP servers that are still connecting, and how often it asks. */
const mcpSettleMs = 10_000
const mcpPollMs = 500

function requireCompatibleBackend(version: string | undefined): void {
  const error = backendVersionError(version ?? "")
  if (error) throw new Error(error)
}

export function createBackendLifecycle({
  client, store, directory, startedRemote, startup, connectionHint, reconnect, find, onRestarted,
  probe = (url) => probeHealth(url, fetch, undefined, url.replace(/\/+$/, "") === client.baseUrl ? client.token : undefined),
  startRemoteBridge, home, onScopeChanged, status, isClosing, refresh, refreshVcs, subscribeFocus,
  startGlobalStream, startStream, stopGlobalStream, leaveOpenSession, activateProject, openProjectOverlay,
  openSession, newSession, resume,
}: BackendOptions) {
  /** No `EnsureProjectForPath`; new sessions need a chosen Project: `--remote`, or connected through `/connect-remote`. */
  let remote = startedRemote
  /** The relay bridge child of `/connect-remote` while it runs (app/run.tsx; src/bridge.ts). */
  let remoteBridge: Bridge | undefined
  /** The bridge child exited on its own: the TUI stays on the (dead) remote until `/connect-remote` or `/disconnect-remote`. */
  let bridgeDown = false
  let connectingRemote = false
  /** The catalogs could not be read when the remote was entered (backend offline): read them when the global stream opens. */
  let catalogStale = false
  /** A remote backend reached through `/connect-remote`'s bridge (running or exited). */
  const viaBridge = (): boolean => remoteBridge !== undefined || bridgeDown || connectingRemote

  /** Set while `start()` runs; a prompt with no session yet waits for it (see `submit`). */
  let starting: Promise<void> | undefined
  /**
   * Initial load: bootstrap, the Project of `--dir` (`EnsureProjectForPath`;
   * skipped with `--remote`, which starts without an active Project),
   * catalogs, then the session `startup` names (`--session <id>`, or
   * `--continue`: the most recent nonarchived root of that Project). A
   * plain local start restores a saved chat, prioritizing pending requests, or creates a new ephemeral session.
   */
  function start(): Promise<void> {
    starting = startOnce().finally(() => { starting = undefined })
    return starting
  }
  /**
   * The startup list's bundles and MCP servers (state/startup.ts). MCP has no
   * push for connection changes: poll `GetMcpStatus` while a server is still
   * connecting, at most `mcpSettleMs`. Failures leave the sections out.
   */
  async function loadStartupResources(): Promise<void> {
    const attempt = async <T>(read: () => Promise<T>): Promise<T | undefined> => {
      try { return await read() } catch { return undefined }
    }
    const bundles = attempt(() => client.listBundles()).then((rows) => { if (rows) store.setBundles(rows) })
    const deadline = Date.now() + mcpSettleMs
    for (;;) {
      const servers = await attempt(() => client.getMcpStatus())
      if (!servers) break
      store.setMcpServers(servers)
      if (!servers.some((server) => server.state === "MCP_SERVER_STATE_DESIRED") || Date.now() >= deadline) break
      await Bun.sleep(mcpPollMs)
    }
    await bundles
    store.setStartupReady(true)
  }

  async function startOnce(): Promise<void> {
    subscribeFocus()
    try {
      // A remote backend has no use for this machine's --dir: no scope until a Project is chosen.
      if (remote) client.setDirectory("")
      const bootstrap = await client.bootstrap()
      requireCompatibleBackend(bootstrap.location?.version)
      store.applyBootstrap(bootstrap)
      store.setRemote(remote)
      let missing = ""
      let ensured: ProjectInfo | undefined
      if (!remote) {
        // A failure (an older backend) leaves no active Project: new sessions then send `--dir` alone.
        ensured = await client.ensureProjectForPath(directory).then((result) => result.project, (error: unknown) => {
          missing += ` · no project for ${directory}: ${String(error)}`
          return undefined
        })
      }
      await refresh()
      if (ensured) activateProject(ensured)
      void refreshVcs()
      void loadStartupResources()
      startGlobalStream()
      const target = initialSessionId(store.state.sessions, startup, store.state.activeProjectId)
      const resumeId = startup.resume?.id
      if (resumeId) await resume(resumeId).catch(() => { missing += ` · session ${resumeId} not found` })
      else if (startup.resume) {
        // The picker waits for a choice; Esc (or nothing to pick) starts a new session.
        await resume(undefined, () => void newSession().catch(() => undefined))
      }
      else if (target) await openSession(target).catch(() => { missing += ` · session ${target} not found` })
      else if (startup.continue) missing += remote ? " · --continue needs a project" : " · no earlier session in this project"
      else if (!remote && store.state.activeProjectId) {
        const projectId = store.state.activeProjectId
        const history = await client.listSessions({ includeArchived: true, projectId }).catch((error: unknown) => {
          missing += ` · could not read earlier sessions: ${String(error)}`
          return [] as SessionInfo[]
        })
        const durable = history.filter((session) => !session.ephemeral)
        const waiting = durable.filter((session) => store.state.interactions.some((ask) => ask.session && treeSessionIds(session.id, history).has(ask.session)))
        const prior = newestTopLevelSession(waiting, projectId, { includeArchived: true })
          ?? newestTopLevelSession(durable, projectId, { includeArchived: true })
        if (prior) await resume(prior.id).catch((error: unknown) => { missing += ` · could not resume ${prior.id}: ${String(error)}` })
        if (!store.state.selected) await newSession().catch(() => undefined)
      }
      // With no local Project or model, no session is created until the first prompt.
      else if (!remote || store.state.activeProjectId) await newSession().catch(() => undefined)
      if (remote && !store.state.selected) missing += ` · ${noProjectStatus}`
      // `--remote`: no Project was ensured; open the Project view so choosing or creating one is the first thing shown
      // (unless `--resume` already shows its picker).
      if (remote && !store.state.activeProjectId && !startup.resume) openProjectOverlay()
      const version = bootstrap.location?.version ?? ""
      const compatibilityError = backendVersionError(version)
      const mismatch = compatibilityError ? ` · ${compatibilityError}` : ""
      // A WebUI that bare `hya` could not start is the one notice worth the status line.
      // Reopening any session already said `Resumed …`; keep it unless something needs saying.
      const resumed = !missing && !mismatch && store.state.status.startsWith("Resumed ")
      if (!resumed) status(webNotice(store.state.web) ?? `Connected to hya ${version} · ? or /help for keys and commands${missing}${mismatch}`)
    } catch (error) {
      status(`Connection failed: ${String(error)} · ${connectionHint}`)
      // Keep the chat surface usable on startup failures; full-screen help is an
      // explicit ? or /help action, not an error fallback.
      store.setView("chat")
      store.markReady()
    }
  }

  /** Move to another server (app/reconnect.ts): new base URL, both streams resubscribed, catalogs and the open session reloaded. */
  async function switchServer(next: ServerSwitch): Promise<void> {
    client.setBaseUrl(next.url)
    store.setServerUrl(next.url)
    try {
      const bootstrap = await client.bootstrap()
      requireCompatibleBackend(bootstrap.location?.version)
      store.applyBootstrap(bootstrap)
    } catch (error) {
      status(`Connection failed: ${String(error)}`)
      return
    }
    await refresh().catch((error: unknown) => status(`Refresh failed: ${String(error)}`))
    void onScopeChanged?.()
    startGlobalStream()
    const selected = store.state.selected
    if (selected) {
      await openSession(selected.id).catch(async (error: unknown) => {
        // Gone while no stream watched it (an unused ephemeral session the daemon dropped, or a
        // delete another client made meanwhile): never stay on a session with no log behind it.
        if (!(error instanceof HttpError && error.status === 404)) return status(`Open failed: ${String(error)}`)
        store.dropSessionRow(selected.id)
        await newSession().catch((failure: unknown) => status(`Session ${selected.id} is gone · new session failed: ${String(failure)}`))
      })
      // A turn that ran on the old server died with it; the transcript above shows how far it got.
      if (store.state.running && !store.state.selected?.busy) store.endTurn()
    }
    const backend = store.state.backend
    if (backend) {
      store.setBackend({
        ...backend,
        pid: next.pid,
        ...(next.version ? { version: next.version } : {}),
        ...(next.startedAt !== undefined ? { startedAt: next.startedAt } : {}),
      })
    }
  }

  const reconnector = reconnect
    ? createReconnector({
      url: () => client.baseUrl,
      generation: () => {
        const backend = store.state.backend
        return backend?.pid !== undefined && backend.startedAt !== undefined ? `${backend.pid}:${backend.startedAt}` : undefined
      },
      probe, reconnect, switchTo: switchServer, status,
      ...(find ? { find } : {}),
      onStopped: (stopped) => { store.setBackendStopped(stopped); if (stopped) store.setConnected(false) },
      ...(onRestarted ? { onRestarted } : {}),
    })
    : undefined

  /**
   * Point the client at `url` (with a relay bridge's `token`; none for a
   * local backend) and load it like a start: bootstrap, the
   * Project of `--dir` unless remote, catalogs, the global stream. A remote
   * opens the Project view (no session is created); a local backend opens a
   * new session in the ensured Project.
   */
  async function enterServer(url: string, token?: string): Promise<{ ok: boolean; detail: string }> {
    client.setBaseUrl(url, token)
    store.setServerUrl(url)
    // A remote backend has no use for this machine's --dir: no scope until a Project is chosen.
    client.setDirectory(remote ? "" : directory)
    store.setActiveProject(undefined)
    let detail = ""
    let ok = true
    try {
      const bootstrap = await client.bootstrap()
      requireCompatibleBackend(bootstrap.location?.version)
      store.applyBootstrap(bootstrap)
      let ensured: ProjectInfo | undefined
      if (!remote) {
        ensured = await client.ensureProjectForPath(directory).then((result) => result.project, (error: unknown) => {
          detail += ` · no project for ${directory}: ${String(error)}`
          return undefined
        })
      }
      await refresh()
      if (ensured) activateProject(ensured)
    } catch (error) {
      ok = false
      catalogStale = true
      detail += ` · ${String(error)}`
    }
    if (ok) {
      startGlobalStream()
      // Local/remote trust changes can replace the pane instance. Finish that
      // replacement before exposing Projects to input on the new backend.
      await onScopeChanged?.()
    }
    if (remote) {
      if (!store.state.activeProjectId) openProjectOverlay()
    } else if (ok) {
      void refreshVcs()
      await newSession().catch(() => undefined)
    }
    return { ok, detail }
  }

  /** The bridge child exited without `/disconnect-remote` asking it to. */
  function bridgeExited(child: Bridge, code: number): void {
    if (child !== remoteBridge || child.stopping || isClosing()) return
    remoteBridge = undefined
    bridgeDown = true
    store.setConnected(false)
    const last = stripTerminalControls(child.lastLine() ?? "").replace(/^(?:error:\s*)?(?:hya bridge:\s*)?/i, "")
    status(`Remote bridge exited (${last ? last : `code ${code}`}) · ${bridgeExitedStatus(home !== undefined)}`)
  }

  /**
   * `/connect-remote <link>`: tear down a previous bridge child, start a new
   * one (src/bridge.ts; the link goes to its stdin only), wait for it
   * (status shows the progress and the bridge's lines), then move to its
   * loopback URL as a remote start: label in the header, no Project ensured,
   * no local reconnects, the Project view open.
   */
  async function connectRemote(link: string, flags: BridgeFlags = {}): Promise<void> {
    if (!startRemoteBridge) {
      status("/connect-remote is not available here (no hya binary to run hya bridge)")
      return
    }
    if (connectingRemote) {
      status("Already connecting to a remote backend…")
      return
    }
    const wasRemote = viaBridge()
    connectingRemote = true
    const started = Date.now()
    let latest = ""
    const progress = (): void => status(`Connecting to the relay… ${Math.round((Date.now() - started) / 1000)}s${latest ? ` · ${latest}` : ""}`)
    const ticker = setInterval(progress, 1000)
    try {
      const previous = remoteBridge
      remoteBridge = undefined
      bridgeDown = false
      if (previous) {
        status("Closing the previous relay bridge…")
        await previous.stop()
      }
      progress()
      let child: Bridge | undefined
      child = await startRemoteBridge(link, flags, (raw) => {
        // src/bridge.ts already strips them; the status line never takes terminal controls.
        const line = stripTerminalControls(raw)
        latest = line.replace(/^(?:error:\s*)?(?:hya bridge:\s*)?/i, "")
        // After start-up the bridge only reports changes (online, offline, relay unreachable).
        if (child && child === remoteBridge) status(line)
        else progress()
      })
      clearInterval(ticker)
      await leaveOpenSession()
      remoteBridge = child
      const running = child
      void child.exited.then((code) => bridgeExited(running, code))
      remote = true
      store.setRemote(true)
      store.setServerLabel(child.label)
      store.setBackend({ remoteBridge: true })
      const entered = await enterServer(child.url, child.token)
      status(entered.ok
        ? `Connected to ${child.label} · choose a project, or t for a temporary session`
        : `Connected to the relay, but the remote backend did not answer${entered.detail} · the TUI loads it when it comes online`)
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      // Still pointed at a remote (the previous bridge is gone): never fall back to a local daemon by itself.
      if (wasRemote) {
        bridgeDown = true
        store.setConnected(false)
      }
      status(`Remote connection failed: ${message} · /connect-remote to try again${wasRemote && home ? " · /disconnect-remote goes back to the local backend" : ""}`)
    } finally {
      clearInterval(ticker)
      connectingRemote = false
    }
  }

  /**
   * `/disconnect-remote`: close the bridge child's stdin (SIGTERM after 2 s),
   * drop the label, and go back to the local backend (\`home\`: the
   * database's daemon, found or started) with the Project of `--dir` and a
   * new session, like a local start.
   */
  async function disconnectRemote(): Promise<void> {
    if (!viaBridge()) {
      status(store.state.serverLabel
        ? `No local backend to go back to: this TUI was started on ${store.state.serverLabel} (hya --connect) · quit and run hya for a local one`
        : "Not connected to a remote backend · /connect-remote <link> connects to one")
      return
    }
    if (!home) {
      status("No local backend to go back to: this TUI was started by hya --connect · quit and run hya for a local one")
      return
    }
    await leaveOpenSession()
    stopGlobalStream()
    const child = remoteBridge
    remoteBridge = undefined
    bridgeDown = false
    status("Closing the relay bridge…")
    await child?.stop()
    if (store.state.extensionOverlay) store.setExtensionOverlay(undefined)
    remote = startedRemote
    store.setRemote(remote)
    store.setServerLabel(undefined)
    catalogStale = false
    status("Switching back to the local backend…")
    let next: ServerSwitch
    try {
      next = await home()
    } catch (error) {
      status(`Local backend unavailable: ${error instanceof Error ? error.message : String(error)} · /reconnect tries again`)
      return
    }
    // Whatever the local server said before (`hya serve stop`) no longer holds for this one.
    reconnector?.reset()
    const entered = await enterServer(next.url)
    status(entered.ok ? `Back on the local backend${next.pid ? ` · pid ${next.pid}` : ""}${entered.detail}` : `Local backend did not answer${entered.detail} · /reconnect tries again`)
  }

  /** `/reconnect`: find or start the database's server now; a fixed `--server` only resubscribes. */
  async function reconnectNow(): Promise<void> {
    if (bridgeDown) {
      status(bridgeExitedStatus(home !== undefined))
      return
    }
    if (viaBridge() || store.state.serverLabel) {
      // The bridge's URL is fixed; never find or start a local daemon from here.
      status(`Reconnecting to ${store.state.serverLabel ?? client.baseUrl}…`)
      startGlobalStream()
      const selected = store.state.selected
      if (selected) startStream(selected.id)
      return
    }
    if (reconnector) return reconnector.reconnectNow()
    status(`Reconnecting to ${client.baseUrl} (--server without --db: no backend to find or start)`)
    startGlobalStream()
    const selected = store.state.selected
    if (selected) startStream(selected.id)
  }

  /** `serverStopping` (live, empty `session`): the last frame before the server ends this stream. */
  function onStopping(event: StreamEvent | undefined): boolean {
    if (!event?.serverStopping) return false
    // A remote backend stopping: its host brings it back; nothing local is found or started.
    if (viaBridge()) status(`Remote backend stopping (${event.serverStopping.reason || "no reason"}) · the TUI reconnects when it is back`)
    else reconnector?.stopping(client.baseUrl, event.serverStopping.reason ?? "")
    return true
  }

  /**
   * A stream failed or ended. A local server may be gone: the reconnector
   * checks and replaces it. Through the relay bridge the URL is fixed (the
   * bridge answers 503 while the remote is offline): the streams keep
   * retrying it, and no local daemon is ever found or started.
   */
  function serverLost(): void {
    if (viaBridge()) return
    void reconnector?.lost()
  }

  function refreshStaleCatalog(): Promise<void> | undefined {
    if (catalogStale) {
      catalogStale = false
      return refresh().catch(() => { catalogStale = true })
    }
  }

  async function stopBridge(): Promise<void> {
    // The bridge child also exits when this process dies (its stdin closes); close it cleanly first.
    const child = remoteBridge
    remoteBridge = undefined
    await child?.stop(500).catch(() => undefined)
  }

  return {
    start, connectRemote, disconnectRemote, reconnectNow, onStopping, serverLost, refreshStaleCatalog, stopBridge,
    get starting() { return starting },
    get remote() { return remote },
    get bridgeDown() { return bridgeDown },
    reconnectBusy: () => reconnector?.busy(),
    reconnectStopped: () => reconnector?.stopped(),
    viaBridge,
  }
}

/** What to do after the relay bridge child exited on its own. */
export function bridgeExitedStatus(local: boolean): string {
  return `/connect-remote <link> connects again${local ? " · /disconnect-remote goes back to the local backend" : ""}`
}
