import { normalizeStreamEvent, sessionMembers, type HyaClient, type MessageInfo, type SessionInfo, type StreamEvent, type StreamFrame } from "../client"
import { askSessionLabel, otherAskNotice } from "../state/format"
import { childActivity } from "../state/members"
import { askFrameRoute, globalAskRoute } from "../state/prompts"
import { sessionRow } from "../state/revert"
import type { AppStore } from "../state/store"
import { subagentRows } from "../state/subagents"
import type { createBackendLifecycle } from "./backendLifecycle"
import { createDebounce } from "./debounce"
import { turnEndStatus, type createTurnRunner } from "./turns"

interface SubscriptionOptions {
  client: HyaClient
  store: AppStore
  backend: Pick<ReturnType<typeof createBackendLifecycle>, "onStopping" | "serverLost" | "reconnectBusy" | "reconnectStopped" | "bridgeDown" | "refreshStaleCatalog">
  turns: Pick<ReturnType<typeof createTurnRunner>, "observe">
  status: (text: string) => void
  notifyAsk: (id: string, kind: "permission" | "question", detail: string) => void
  refreshProjects: () => Promise<void>
  refreshVcs: () => Promise<void>
  newSession: () => Promise<void>
}

/** Overlay flush interval: coalesces stream deltas into one render per display frame. */
const flushMs = 16
/** Projection re-read debounce: after 120 ms of quiet, but at least every 400 ms while frames keep coming. */
const refreshWaitMs = 120
const refreshMaxWaitMs = 400

/** Child-session re-read interval while a child is busy or a turn runs. */
export const childPollMs = 1500
/** Global stream reconnect backoff: the session stream's 800 ms, doubling up to 15 s while it keeps failing (an older backend without the route). */
const globalRetryMs = 800
const globalRetryMaxMs = 15_000

export function createSubscriptions({ client, store, backend, turns, status, notifyAsk, refreshProjects, refreshVcs, newSession }: SubscriptionOptions) {

  let streamAbort: AbortController | undefined
  let globalAbort: AbortController | undefined
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

  /** `projectsUpdated` (live, global stream only): one `ListProjects` per burst. */
  const projectsRefreshLater = createDebounce(() => {
    void refreshProjects().catch(() => undefined)
  }, { wait: refreshWaitMs, maxWait: refreshMaxWaitMs })

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

  /** Re-read the open session's row: `SessionInfo.usage` after a `tokensRecorded` (E22). */
  async function refreshSession(): Promise<void> {
    const selected = store.state.selected
    if (!selected) return
    const row = await client.request<SessionInfo>("GET", `/v2/sessions/${encodeURIComponent(selected.id)}`)
    const current = store.state.selected
    if (current?.id === row.id) {
      store.setSelected(sessionRow(current, row))
      store.setMembers(sessionMembers(row))
    }
  }

  /** `GET /v2/interactions` into the pending list (after a (re)subscribe or `resync`: frames before it are not replayed). */
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

  /**
   * Re-read the provider/model catalog (`GET /v2/models` / `GET /v2/providers`)
   * and the open session's row: lighter than `refresh()`, so a
   * `catalogUpdated` frame does not also disturb the session list or
   * interactions. The session row carries the server-resolved effort, which a
   * saved effort choice (`SetModelEffortPreference`, `SetAgentEffort`, from any
   * client) changes, so its `model:effort` label updates live.
   */
  async function refreshCatalogOnly(): Promise<void> {
    const [models, providers] = await Promise.all([client.listModels(), client.listProviders(), refreshSession()])
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

  /**
   * `sessionStarted` (a creation this client missed) or a `sessionUpdated`
   * for a session it has not listed yet: debounced so a burst of several
   * (a script creating many sessions, a fork tree) is one re-read, matching
   * `catalogRefreshLater`'s reasoning. The default listing hides archived,
   * matching the sidebar's own (`docs/protocol/README.md` "Session list push").
   */
  const sessionListRefreshLater = createDebounce(() => {
    void client.listSessions().then((rows) => store.setSessions(rows)).catch(() => undefined)
  }, { wait: refreshWaitMs, maxWait: refreshMaxWaitMs })

  function scheduleSessionListRefresh(): void {
    sessionListRefreshLater.schedule()
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
    const generation = childGeneration
    const [session, messages] = await Promise.all([
      client.request<SessionInfo>("GET", `/v2/sessions/${encodeURIComponent(id)}`),
      client.listMessages(id),
    ])
    if (generation !== childGeneration || closing) return
    const last = [...messages].reverse().find((message: MessageInfo) => message.role === "ROLE_ASSISTANT")
    const activity = childActivity(messages)
    store.setChild(id, {
      busy: session.busy === true,
      agent: session.agent,
      members: sessionMembers(session),
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
    const ids = subagentRows(store.state, { includeArchived: true }).map((row) => row.id)
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
      // The listing may discover another generation of descendants after the
      // reads began. Load their member snapshots even if the whole team is idle.
      const discovered = subagentRows(store.state, { includeArchived: true }).some((row) => !ids.includes(row.id))
      if (childAgain || busy || discovered || store.state.running) trackChildren()
    })
  }

  function applyEvent(rawEvent: StreamEvent): void {
    const event = normalizeStreamEvent(rawEvent)
    // Live, no seq, empty `session`: the provider/model catalog changed. Not
    // a transcript or ask frame, so it never reaches the store's fold —
    // debounced here instead of `scheduleRefresh()`'s projection re-read.
    if (event.catalogUpdated) {
      scheduleCatalogRefresh()
      return
    }
    if (event.projectsUpdated) {
      projectsRefreshLater.schedule()
      return
    }
    const effect = store.applyEvent(event)
    if ((event.memberUpdated || event.memberLifecycleChanged) && effect.durable) trackChildren()
    if (event.memberLifecycleChanged && effect.durable) sessionDue = true
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
    if (backend.onStopping(frame.event)) return
    if (store.state.selected?.id !== sessionId) return
    if (frame.resync) {
      store.markLiveLost()
      await gapFill(sessionId)
      // Live ask frames in the gap are lost too.
      await refreshInteractions().catch(() => undefined)
      scheduleRefresh()
      return
    }
    const event = frame.event ? normalizeStreamEvent(frame.event) : undefined
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
          if (!controller.signal.aborted && !backend.reconnectBusy()) store.setConnected(false)
          // A stopped TUI keeps its `Backend stopped` notice while the stream retries.
          if (!controller.signal.aborted && !backend.reconnectBusy() && !backend.reconnectStopped() && !backend.bridgeDown) status(`Stream reconnecting: ${String(error)}`)
        }
        ready()
        // Ended or failed: the server may be gone (stopped, restarted, crashed).
        if (!controller.signal.aborted && !closing) backend.serverLost()
        if (!controller.signal.aborted) await Bun.sleep(800)
      }
    })()
  }

  /**
   * One frame of the global stream: asks and resolves, plus (for root
   * sessions) `sessionStarted`/`sessionUpdated`/`sessionDeleted`
   * (`docs/protocol/README.md` "Session list push") that keep the sidebar
   * and open `/sessions` / `/resume` pickers current without polling. The
   * open tree's asks are also on its session stream (applied there; kept by
   * id here too). An ask of another session is shown in the pending block
   * with its session, announced on the status line, and notified while
   * unfocused.
   */
  async function onGlobalFrame(frame: StreamFrame): Promise<void> {
    if (backend.onStopping(frame.event)) return
    if (frame.resync) {
      // Recover list, ask, and team-root lifecycle frames lost in the gap.
      await Promise.allSettled([
        client.listSessions().then((rows) => store.setSessions(rows)),
        refreshInteractions(), refreshSession(),
      ])
      trackChildren()
      return
    }
    const event = frame.event ? normalizeStreamEvent(frame.event) : undefined
    if (!event) return
    // Live, no seq, empty `session`: never an ask, so `globalAskRoute` would
    // ignore it; debounced with the copy that may also arrive on the open
    // session's own stream (`applyEvent`), so a burst on both is one re-read.
    if (event.catalogUpdated) {
      scheduleCatalogRefresh()
      return
    }
    // Live, no seq, empty `session`: the Project list (or a Project's `busy`) changed.
    if (event.projectsUpdated) {
      projectsRefreshLater.schedule()
      return
    }
    const sessionId = event.session
    // Actor lifecycle belongs to the team root, even when this client has
    // opened a nested parent. Re-read projections without applying another
    // session's sequence watermark to the open transcript.
    if (event.memberLifecycleChanged) {
      const child = event.memberLifecycleChanged.child
      if (sessionId === store.state.selected?.id || subagentRows(store.state, { includeArchived: true }).some((row) => row.id === child)) {
        sessionDue = true
        scheduleRefresh()
        trackChildren()
      }
      return
    }
    // A session created elsewhere (another client, a headless writer, a fork): not enough here
    // (agent/model/workdir, no title yet) to build a row cheaply, so re-list, debounced.
    if (sessionId && event.sessionStarted) {
      if (!store.state.sessions.some((row) => row.id === sessionId)) scheduleSessionListRefresh()
      return
    }
    // Deleted elsewhere: drop the row; if it was the open one, never leave the
    // TUI stuck on a session with no log behind it — show a notice and open a fresh one.
    if (sessionId && event.sessionDeleted) {
      // This client's own delete (`deleteSession` above): its own flow already
      // dropped the row and navigated (native.ts's `/sessions` Ctrl+D) — this
      // echo just confirms it, so only the `selfDeleting` mark is consumed.
      const own = selfDeleting.delete(sessionId)
      const wasOpen = store.state.selected?.id === sessionId
      store.dropSessionRow(sessionId)
      if (wasOpen && !own) {
        // `newSession()` ends with its own `status("Created …")`: set this client's
        // notice after it settles, not before, so the notice is what is left on
        // screen (both calls are sequential, never concurrent — no race to lose).
        await newSession().catch((error: unknown) => status(`Session ${sessionId} was deleted elsewhere · new session failed: ${String(error)}`))
        if (store.state.selected?.id !== sessionId) status(`Session ${sessionId} was deleted elsewhere; opened a new session`)
      }
      return
    }
    const updated = event.sessionUpdated
    if (sessionId && updated) {
      // Another client archived or unarchived a session: drop or mark its sidebar row.
      if (updated.archived !== undefined) {
        store.applyArchived(sessionId, updated.archived)
        if (!updated.archived && !store.state.sessions.some((row) => row.id === sessionId)) scheduleSessionListRefresh()
        return
      }
      // title/agent/model/permissionMode/busy: patch the row (idempotent with the
      // open session's own-stream copy, `patchSessionRow`'s doc comment); an id not
      // listed yet (this frame outran the initial listing) is a reason to re-list.
      if (!store.patchSessionRow(sessionId, updated)) scheduleSessionListRefresh()
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
    status(otherAskNotice(asked, store.state.sessions, store.state.activeProjectId))
    const where = asked.session ? ` · in ${askSessionLabel(asked.session, store.state.sessions, store.state.activeProjectId)}` : ""
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
            // A remote that was offline when it was entered: its catalogs now.
            const catalog = backend.refreshStaleCatalog()
            if (catalog) await catalog
            // Asks and root lifecycle notifications before this subscription
            // are not replayed here; refresh nested parents' member state too.
            await Promise.allSettled([refreshInteractions(), refreshSession()])
            trackChildren()
          })
        } catch {
          // Silent: the session stream owns the connection state; this one only adds other sessions' asks.
        }
        if (controller.signal.aborted) break
        // With no session open this is the only stream: it notices a lost server too.
        if (!closing) backend.serverLost()
        await new Promise<void>((resolve) => {
          const timer = setTimeout(resolve, delay)
          controller.signal.addEventListener("abort", () => { clearTimeout(timer); resolve() }, { once: true })
        })
        delay = Math.min(delay * 2, globalRetryMaxMs)
      }
    })()
  }

  /** Ids this client is deleting itself (`deleteSession` below): `onGlobalFrame`'s `sessionDeleted` echo of one of these is not "deleted elsewhere". */
  const selfDeleting = new Set<string>()

  /** `AppActions.deleteSession` (see its doc comment): marks `id` self-deleted for up to 20 s (well past the global stream's delivery), then deletes it. */
  async function deleteSession(id: string): Promise<void> {
    selfDeleting.add(id)
    setTimeout(() => selfDeleting.delete(id), 20_000)
    await client.deleteSession(id)
  }

  function resetChildren(): void {
    childGeneration++
    if (childTimer) clearTimeout(childTimer)
    childTimer = undefined
    childReading = false
    childAgain = false
    lastChildRead = 0
  }

  function stopSessionStream(): void {
    streamAbort?.abort()
    streamAbort = undefined
  }

  const stopGlobalStream = (): void => globalAbort?.abort()

  function dispose(): void {
    closing = true
    streamAbort?.abort()
    globalAbort?.abort()
    if (childTimer) clearTimeout(childTimer)
    refreshLater.cancel()
    projectsRefreshLater.cancel()
    catalogRefreshLater.cancel()
    if (flushTimer) clearTimeout(flushTimer)
  }

  return {
    refreshMessages, scheduleRefresh, resetChildren, startStream, startGlobalStream,
    stopSessionStream, stopGlobalStream, deleteSession, dispose,
    get closing() { return closing },
    get streamReady() { return streamReady },
  }
}
