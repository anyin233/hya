import { expect, test } from "bun:test"
import type { HyaClient, Interaction, MessageInfo, ProjectInfo, SessionInfo, StreamFrame, StreamEvent } from "../src/client"
import { createController } from "../src/app/controller"
import type { ServerSwitch } from "../src/app/reconnect"
import { transcriptViews } from "../src/state/messages"
import { createAppStore } from "../src/state/store"

/**
 * `hya serve restart` end to end at the controller (app/reconnect.ts's
 * `serverStopping {restart}` path): the successor serves the same URL and
 * session, so the TUI must keep the session open on it, render the turn that
 * is durable there, and re-read everything the new daemon owns. The
 * reconnector's state machine alone is unit-tested in test/reconnect.test.ts;
 * these tests drive `createController`'s own switch (`switchServer`).
 *
 * Every wait awaits a real signal (the successor's stream subscription, a
 * status line, a delivered frame). The one unavoidable real delay is the
 * reconnector's 500 ms probe gap, which `createController` does not inject.
 */

const url = "http://127.0.0.1:3000"
const oldPid = 1000
const nextPid = 4242
const work: ProjectInfo = { id: "prj_work", name: "work", roots: ["/work"], busy: false }

function harness() {
  const store = createAppStore()
  const statuses: string[] = []
  /** The one status waiter, if any; each new status is offered to it once. */
  let announce: ((body: string) => void) | undefined
  const setStatus = store.setStatus.bind(store)
  store.setStatus = (body: string) => {
    statuses.push(body)
    setStatus(body)
    const wake = announce
    announce = undefined
    wake?.(body)
  }
  /** The next status satisfying `match`, or one already on the line. */
  const statusWhere = (match: (body: string) => boolean): Promise<string> => {
    const seen = statuses.find(match)
    if (seen !== undefined) return Promise.resolve(seen)
    return new Promise<string>((resolve) => {
      const wait = (body: string): void => {
        if (match(body)) resolve(body)
        else announce = wait
      }
      announce = wait
    })
  }

  const calls: Array<[string, ...unknown[]]> = []
  /** False while the old daemon (pid 1000) serves; true once the successor does. */
  let restarted = false
  const sessions: SessionInfo[] = []
  /** Durable events the successor's log replays after its `lastSeq` (empty before it exists). */
  const replay: StreamEvent[] = []
  const ask: Interaction = { id: "perm_1", session: "s1", type: "INTERACTION_TYPE_PERMISSION", title: "bash echo resumed" }
  let created = 0

  const projection = (): MessageInfo[] => (restarted
    ? [
      { id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "m_u_p", text: { text: "hi" } }] },
      { id: "m_a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "m_a_p", text: { text: "resumed answer" } }] },
    ]
    : [{ id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "m_u_p", text: { text: "hi" } }] }])

  /** The open session's stream: `push` delivers a frame (awaitable), `end` ends the stream (the server went away). */
  type SessionStream = { session: string; since: string; push: (frame: StreamFrame) => Promise<void>; end: () => void }
  const sessionStreams: SessionStream[] = []
  let streamOpened: ((stream: SessionStream) => void) | undefined
  /** The next session stream that subscribes (the initial one, then the successor's). */
  const nextStream = (): Promise<SessionStream> => {
    const registered = sessionStreams[sessionStreams.length]
    if (registered) return Promise.resolve(registered)
    return new Promise<SessionStream>((resolve) => { streamOpened = resolve })
  }
  const globalEnds: Array<() => void> = []

  const client = {
    get baseUrl() { return url },
    setBaseUrl() { /* the restart plan keeps one URL */ },
    get token() { return undefined },
    get directory() { return "/work/sub" },
    setDirectory() {},
    bootstrap: async () => {
      calls.push(["bootstrap"])
      return {
        location: { version: restarted ? "next" : "old", pid: restarted ? nextPid : oldPid },
        agents: [{ name: "build" }],
        models: [{ id: "hya/echo", providerId: "hya", modelId: "echo" }],
      }
    },
    ensureProjectForPath: async () => ({ project: work, created: false }),
    listProjects: async () => [work],
    listSessions: async () => sessions,
    listInteractions: async (): Promise<Interaction[]> => (restarted ? [ask] : []),
    listModels: async () => [restarted ? { id: "hya/next", providerId: "hya", modelId: "next" } : { id: "hya/echo", providerId: "hya", modelId: "echo" }],
    listAgents: async () => [{ name: "build" }],
    listWorkflows: async () => [],
    listProviders: async () => [],
    listCommands: async () => [],
    listPermissionModes: async () => [],
    getVcsStatus: async () => ({}),
    getSessionTodo: async () => [],
    listMessages: async (): Promise<MessageInfo[]> => projection(),
    listEventsSince: async (_session: string, since: string) => {
      calls.push(["listEventsSince", since])
      return replay
    },
    request: async (_method: string, path: string) => {
      const id = decodeURIComponent(path.split("/").at(-1) ?? "")
      const row = sessions.find((session) => session.id === id)
      if (!row) throw new Error(`not found ${id}`)
      return row
    },
    createSession: async (agent: string, _model: unknown, placement: { projectId?: string }) => {
      calls.push(["createSession"])
      const session: SessionInfo = { id: `s${++created}`, agent, workdir: "/work", model: { providerId: "hya", modelId: "echo" }, ...(placement.projectId ? { projectId: placement.projectId } : {}) }
      sessions.unshift(session)
      return session
    },
    streamGlobal: (_onFrame: (frame: StreamFrame) => void, signal: AbortSignal, onOpen?: () => void | Promise<void>) => {
      calls.push(["streamGlobal"])
      void onOpen?.()
      return new Promise<void>((resolve) => {
        globalEnds.push(resolve)
        signal.addEventListener("abort", () => resolve(), { once: true })
      })
    },
    streamSession: async (
      session: string,
      since: string,
      onFrame: (frame: StreamFrame) => void | Promise<void>,
      signal: AbortSignal,
      onOpen?: () => void | Promise<void>,
    ) => {
      calls.push(["streamSession", session, since])
      await onOpen?.()
      const stream: SessionStream = {
        session,
        since,
        push: async (frame) => { await onFrame(frame) },
        end: () => undefined,
      }
      const done = new Promise<void>((resolve) => {
        stream.end = resolve
        signal.addEventListener("abort", () => resolve(), { once: true })
      })
      sessionStreams.push(stream)
      const opened = streamOpened
      streamOpened = undefined
      opened?.(stream)
      return done
    },
  }

  const controller = createController({
    client: client as unknown as HyaClient,
    store,
    directory: "/work/sub",
    probe: async () => false,
    // The restart never starts one; only `find` (the successor) answers.
    reconnect: async (): Promise<ServerSwitch> => { throw new Error("restart must never start a server") },
    find: async (): Promise<ServerSwitch> => {
      calls.push(["find"])
      restarted = true
      const open = sessions[0]!
      open.lastSeq = "12"
      open.busy = false
      replay.push(
        { seq: "13", session: open.id, messageStarted: { message: "m_b", role: "ROLE_ASSISTANT" } },
        { seq: "14", session: open.id, partStarted: { message: "m_b", part: "p_b", kind: "text" } },
        { seq: "15", session: open.id, partAppended: { message: "m_b", part: "p_b", textDelta: "still going" } },
        { seq: "16", session: open.id, messageFinished: { message: "m_b", finish: "FINISH_REASON_STOP" } },
      )
      return { url, pid: nextPid, started: false, version: "next", startedAt: 5 }
    },
  })

  /** `serverStopping {restart}` as the last frame, then the stream ends. */
  const restart = (stream: SessionStream): void => {
    stream.push({ event: { session: stream.session, serverStopping: { reason: "restart" } } }).then(() => stream.end())
  }

  return { store, controller, statuses, calls, sessions, sessionStreams, restart, nextStream, statusWhere }
}

test("after `serverStopping {restart}` the TUI attaches to the successor at the same URL: the session stays open, its durable turn renders, and no phantom turn keeps running", async () => {
  const h = harness()
  await h.controller.start()
  const first = await h.nextStream()
  const id = h.store.state.selected?.id
  expect(id).toBe("s1")
  // A turn was running when the server went away.
  h.store.beginTurn()
  expect(h.store.state.running).toBe(true)

  h.restart(first)
  const successor = await h.nextStream()

  // Same session, reopened on the successor at the same URL (a new daemon: the successor's bootstrap pid).
  expect(successor.session).toBe(id)
  expect(successor.since).toBe("12")
  expect(h.store.state.serverUrl).toBe(url)
  expect(h.store.state.serverPid).toBe(4242)
  expect(h.store.state.serverVersion).toBe("next")
  // Its projection renders the turn that is durable on the successor.
  expect(h.store.state.messages.map((message) => message.id)).toEqual(["m_u", "m_a"])
  // The stale turn is not running: the TUI does not wait on a turn the old server took with it.
  expect(h.store.state.running).toBe(false)
  // It said it was waiting, and it said where it landed.
  expect(h.statuses).toContain("Backend restarting (hya serve restart) · waiting for the new one…")
  expect(await h.statusWhere((body) => body.startsWith("Server moved"))).toBe("Server moved · now pid 4242")
})

test("the successor's log replay and a duplicate of it render once", async () => {
  const h = harness()
  await h.controller.start()
  h.restart(await h.nextStream())
  const successor = await h.nextStream()
  // The gap since the successor's lastSeq was replayed once.
  expect(h.calls.filter(([name]) => name === "listEventsSince").at(-1)).toEqual(["listEventsSince", "12"])

  // A reconnect overlap redelivers durable frames the fold already applied: no second copy.
  await successor.push({ event: { seq: "13", session: "s1", messageStarted: { message: "m_b", role: "ROLE_ASSISTANT" } } })
  await successor.push({ event: { seq: "15", session: "s1", partAppended: { message: "m_b", part: "p_b", textDelta: "still going" } } })
  h.store.flushOverlay()

  const shown = JSON.stringify(transcriptViews(h.store.state))
  expect(shown.split("still going").length - 1).toBe(1)
  expect(shown.split("resumed answer").length - 1).toBe(1)
  // The projection is untouched by the replay; the resumed part lives in the overlay only.
  expect(h.store.state.messages.map((message) => message.id)).toEqual(["m_u", "m_a"])
})

test("pending asks and the catalogs are re-read from the successor, and both streams resubscribe", async () => {
  const h = harness()
  await h.controller.start()
  const first = await h.nextStream()
  expect(h.store.state.interactions).toEqual([])

  h.restart(first)
  await h.nextStream()

  // `switchServer` re-read the catalogs and the pending list (the successor's stream is subscribed).
  expect(h.store.state.interactions.map((row) => row.id)).toEqual(["perm_1"])
  expect(h.store.state.models.map((row) => row.id)).toEqual(["hya/next"])
  expect(h.calls.filter(([name]) => name === "streamGlobal").length).toBe(2)
  expect(h.calls.filter(([name]) => name === "bootstrap").length).toBe(2)
})
