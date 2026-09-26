import { expect, test } from "bun:test"
import type { Bridge, BridgeFlags } from "../src/bridge"
import type { HyaClient, ProjectInfo, SessionInfo, SessionPlacement, StreamFrame } from "../src/client"
import { bridgeDownPromptStatus, createController, relayLinkRefusedStatus, secretEntryHint } from "../src/app/controller"
import type { ServerSwitch } from "../src/app/reconnect"
import { createAppStore } from "../src/state/store"

const link = "hya+insecure://127.0.0.1:8766/room123#SECRETKEY.SECRETPSK"
const label = "remote: 127.0.0.1:8766/room123"
const localUrl = "http://127.0.0.1:3000"
const bridgeUrl = "http://127.0.0.1:40001"
const work: ProjectInfo = { id: "prj_work", name: "work", roots: ["/work"], busy: false }
const remoteProject: ProjectInfo = { id: "prj_remote", name: "remote-project", roots: ["/srv/app"], busy: false }

/** A fake bridge child as the controller sees it; `crash()` ends it without `stop()`. */
function fakeBridge(url = bridgeUrl) {
  let exit!: (code: number) => void
  const exited = new Promise<number>((resolve) => (exit = resolve))
  let stopping = false
  let stops = 0
  const bridge: Bridge = {
    url, room: "room123", proxy: "hya+insecure://127.0.0.1:8766", label,
    exited,
    get stopping() { return stopping },
    lastLine: () => "hya bridge: relay hya+insecure://127.0.0.1:8766 is unreachable",
    stop: async () => { stopping = true; stops++; exit(0) },
  }
  return { bridge, crash: () => exit(1), get stops() { return stops } }
}

/** A fake server per base URL (local and remote), recording calls with the URL they went to. */
function harness(options: { home?: boolean; bridgeError?: string; remote?: boolean; label?: string } = {}) {
  const store = createAppStore()
  const statuses: string[] = []
  const setStatus = store.setStatus.bind(store)
  store.setStatus = (text: string) => { statuses.push(text); setStatus(text) }
  const calls: Array<[string, string, ...unknown[]]> = []
  let base = localUrl
  let directory = "/work/sub"
  let created = 0
  const sessions: SessionInfo[] = []
  const bridges: Array<ReturnType<typeof fakeBridge>> = []
  const bridgeCalls: Array<{ link: string; flags: BridgeFlags }> = []
  const reconnects: string[] = []
  const homes: string[] = []
  const globalEnds: Array<() => void> = []
  const untilAbort = (signal: AbortSignal) => new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }))
  const projectsOf = () => (base === localUrl ? [work] : [remoteProject])
  const record = (name: string, ...rest: unknown[]) => calls.push([name, base, ...rest])
  const client = {
    get baseUrl() { return base },
    setBaseUrl(url: string) { base = url.replace(/\/+$/, "") },
    get directory() { return directory },
    setDirectory(next: string) { directory = next },
    bootstrap: async () => { record("bootstrap"); return { location: { version: "test" }, agents: [{ name: "build" }], models: [{ id: "hya/echo", providerId: "hya", modelId: "echo" }] } },
    ensureProjectForPath: async (path: string) => { record("ensureProjectForPath", path); return { project: work, created: false } },
    listProjects: async () => projectsOf(),
    getProject: async (id: string) => projectsOf().find((row) => row.id === id)!,
    listSessions: async () => sessions.filter((row) => (base === localUrl ? row.projectId !== "prj_remote" : row.projectId === "prj_remote")),
    listInteractions: async () => [],
    listModels: async () => [{ id: "hya/echo", providerId: "hya", modelId: "echo" }],
    listAgents: async () => [{ name: "build" }],
    listWorkflows: async () => [],
    listProviders: async () => [],
    listCommands: async () => [],
    listPermissionModes: async () => [],
    getVcsStatus: async () => ({}),
    getSessionTodo: async () => [],
    listMessages: async () => [],
    listEventsSince: async () => [],
    deleteSession: async (id: string) => { record("deleteSession", id) },
    createTurn: async (session: string, text: string) => { record("createTurn", session, text); return { id: "turn_1" } },
    request: async (_method: string, path: string) => {
      const id = decodeURIComponent(path.split("/").at(-1) ?? "")
      const row = sessions.find((session) => session.id === id)
      if (!row) throw new Error(`not found ${id}`)
      return row
    },
    createSession: async (agent: string, _model: string, placement: SessionPlacement) => {
      record("createSession", placement)
      const projectId = "projectId" in placement ? placement.projectId : undefined
      const session: SessionInfo = { id: `s${++created}`, agent, workdir: "/work", model: { providerId: "hya", modelId: "echo" }, ...(projectId ? { projectId } : {}) }
      sessions.unshift(session)
      return session
    },
    streamGlobal: (_onFrame: (frame: StreamFrame) => void, signal: AbortSignal, onOpen?: () => void | Promise<void>) => {
      record("streamGlobal")
      void onOpen?.()
      return new Promise<void>((resolve) => {
        globalEnds.push(resolve)
        signal.addEventListener("abort", () => resolve(), { once: true })
      })
    },
    streamSession: async (session: string, _since: string, _onFrame: unknown, signal: AbortSignal, onOpen?: () => void | Promise<void>) => {
      record("streamSession", session)
      await onOpen?.()
      return untilAbort(signal)
    },
  }
  const controller = createController({
    client: client as unknown as HyaClient,
    store,
    directory: "/work/sub",
    ...(options.remote ? { remote: true } : {}),
    probe: async () => false,
    reconnect: async (): Promise<ServerSwitch> => { reconnects.push(base); return { url: localUrl, pid: 7, started: true } },
    find: async () => undefined,
    bridge: async (given, flags, onLine) => {
      bridgeCalls.push({ link: given, flags })
      onLine("hya bridge: relay hya+insecure://127.0.0.1:8766: grpc binding")
      if (options.bridgeError) throw new Error(options.bridgeError)
      const next = fakeBridge()
      bridges.push(next)
      return next.bridge
    },
    ...(options.home === false ? {} : { home: async (): Promise<ServerSwitch> => { homes.push(base); return { url: localUrl, pid: 99, started: false } } }),
  })
  if (options.label) store.setServerLabel(options.label)
  return {
    store, controller, calls, statuses, sessions, bridges, bridgeCalls, reconnects, homes,
    get base() { return base },
    named: (name: string) => calls.filter((call) => call[0] === name),
    /** End every open global stream (the server went away). */
    dropStreams: () => { for (const end of globalEnds.splice(0)) end() },
    /** Everything the user could have seen or that was kept. */
    visible: () => JSON.stringify({ statuses, state: store.state }),
  }
}

test("/connect-remote <link> starts the bridge and moves to it as a remote start", async () => {
  const h = harness()
  await h.controller.start()
  const localSession = h.store.state.selected?.id
  expect(localSession).toBe("s1")
  await h.controller.submit(`/connect-remote --transport ws ${link}`)
  expect(h.bridgeCalls).toEqual([{ link, flags: { transport: "ws" } }])
  expect(h.base).toBe(bridgeUrl)
  expect(h.store.state.serverUrl).toBe(bridgeUrl)
  expect(h.store.state.serverLabel).toBe(label)
  expect(h.store.state.remote).toBe(true)
  expect(h.store.state.activeProjectId).toBeUndefined()
  expect(h.store.state.selected).toBeUndefined()
  expect(h.store.state.projectView).toBeDefined()
  expect(h.store.state.projects.map((row) => row.id)).toEqual(["prj_remote"])
  // The empty local session this client created is dropped on the local server.
  expect(h.named("deleteSession")).toEqual([["deleteSession", localUrl, "s1"]])
  // Remote: no Project ensured, no session created there.
  expect(h.named("ensureProjectForPath").filter((call) => call[1] === bridgeUrl)).toEqual([])
  expect(h.named("createSession").filter((call) => call[1] === bridgeUrl)).toEqual([])
  expect(h.store.state.status).toBe(`Connected to ${label} · choose a project, or t for a temporary session`)
  // A prompt without a Project is refused remotely.
  await h.controller.submit("hello")
  expect(h.named("createSession").filter((call) => call[1] === bridgeUrl)).toEqual([])
  // Choosing the remote Project creates the session there.
  await h.controller.switchProject("prj_remote")
  expect(h.named("createSession").at(-1)).toEqual(["createSession", bridgeUrl, { projectId: "prj_remote" }])
  expect(h.visible()).not.toContain("SECRET")
  h.controller.dispose()
})

test("the link never reaches a status line, the store, or the backend", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.submit(`/connect-remote ${link}`)
  // A prompt holding a link is refused and never sent.
  await h.controller.switchProject("prj_remote")
  await h.controller.submit(`here is my link ${link}`)
  expect(h.store.state.status).toBe(relayLinkRefusedStatus)
  expect(h.named("createTurn")).toEqual([])
  // A mistyped command with a link is refused too (it would run as a backend command turn).
  await h.controller.submit(`/connect-remot ${link}`)
  expect(h.store.state.status).toBe(relayLinkRefusedStatus)
  expect(JSON.stringify(h.calls)).not.toContain("SECRET")
  expect(h.visible()).not.toContain("SECRET")
  h.controller.dispose()
})

test("status lines show the label, never the loopback bridge URL", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.submit(`/connect-remote ${link}`)
  await h.controller.reconnect()
  expect(h.store.state.status).toBe(`Reconnecting to ${label}…`)
  h.store.setStatus("")
  // A controller status naming the URL is shown with the label instead.
  await h.controller.submit(`/connect-remote --transport bogus`)
  for (const text of h.statuses.slice(-3)) expect(text).not.toContain("40001")
  h.controller.dispose()
})

test("remote mode never runs the database reconnector, even when the streams fail", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.submit(`/connect-remote ${link}`)
  h.dropStreams()
  await Bun.sleep(50)
  expect(h.reconnects).toEqual([])
  await h.controller.reconnect()
  expect(h.reconnects).toEqual([])
  h.controller.dispose()
})

test("a failing bridge leaves the TUI where it was and says why", async () => {
  const h = harness({ bridgeError: "the remote backend rejected the relay link hya+insecure://127.0.0.1:8766/room123 (rotated or wrong link); ask for a new one" })
  await h.controller.start()
  await h.controller.submit(`/connect-remote ${link}`)
  expect(h.base).toBe(localUrl)
  expect(h.store.state.serverLabel).toBeUndefined()
  expect(h.store.state.remote).toBe(false)
  expect(h.store.state.selected?.id).toBe("s1")
  expect(h.store.state.status).toStartWith("Remote connection failed: the remote backend rejected the relay link")
  expect(h.store.state.status).toEndWith("/connect-remote to try again")
  expect(h.visible()).not.toContain("SECRET")
  h.controller.dispose()
})

test("a bridge that exits on its own is reported; prompts are refused; nothing local is started", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.submit(`/connect-remote ${link}`)
  await h.controller.switchProject("prj_remote")
  h.bridges[0]!.crash()
  await Bun.sleep(0)
  expect(h.store.state.status).toStartWith("Remote bridge exited (relay hya+insecure://127.0.0.1:8766 is unreachable) · /connect-remote <link> connects again")
  expect(h.store.state.status).toContain("/disconnect-remote goes back to the local backend")
  expect(h.store.state.connected).toBe(false)
  await h.controller.submit("hello")
  expect(h.store.state.status).toBe(bridgeDownPromptStatus)
  expect(h.named("createTurn")).toEqual([])
  h.dropStreams()
  await Bun.sleep(50)
  expect(h.reconnects).toEqual([])
  // Connecting again starts a new bridge.
  await h.controller.submit(`/connect-remote ${link}`)
  expect(h.bridges.length).toBe(2)
  expect(h.store.state.status).toStartWith(`Connected to ${label}`)
  h.controller.dispose()
})

test("a second /connect-remote tears the previous bridge down first", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.submit(`/connect-remote ${link}`)
  await h.controller.submit(`/connect-remote ${link}`)
  expect(h.bridges.length).toBe(2)
  expect(h.bridges[0]!.stops).toBe(1)
  expect(h.bridges[1]!.stops).toBe(0)
  // The first bridge's exit (asked for) is not reported as a crash.
  await Bun.sleep(0)
  expect(h.store.state.status).not.toContain("Remote bridge exited")
  h.controller.dispose()
})

test("/disconnect-remote stops the bridge and goes back to the local backend like a local start", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.submit(`/connect-remote ${link}`)
  await h.controller.submit("/disconnect-remote")
  expect(h.bridges[0]!.stops).toBe(1)
  expect(h.homes.length).toBe(1)
  expect(h.base).toBe(localUrl)
  expect(h.store.state.serverLabel).toBeUndefined()
  expect(h.store.state.remote).toBe(false)
  expect(h.store.state.activeProjectId).toBe("prj_work")
  expect(h.named("ensureProjectForPath").at(-1)).toEqual(["ensureProjectForPath", localUrl, "/work/sub"])
  expect(h.named("createSession").at(-1)).toEqual(["createSession", localUrl, { projectId: "prj_work", workdir: "/work/sub" }])
  expect(h.store.state.status).toBe("Back on the local backend · pid 99")
  expect(h.store.state.projectView).toBeUndefined()
  h.controller.dispose()
})

test("/disconnect-remote without a local backend (bare hya --connect) explains and keeps the remote", async () => {
  const h = harness({ home: false, remote: true, label })
  await h.controller.start()
  await h.controller.submit("/disconnect-remote")
  expect(h.store.state.status).toContain("No local backend to go back to")
  expect(h.store.state.serverLabel).toBe(label)
  await h.controller.submit(`/connect-remote ${link}`)
  await h.controller.submit("/disconnect-remote")
  expect(h.store.state.status).toContain("No local backend to go back to")
  expect(h.bridges[0]!.stops).toBe(0)
  expect(h.base).toBe(bridgeUrl)
  h.controller.dispose()
})

test("/disconnect-remote when not connected says so", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.submit("/disconnect-remote")
  expect(h.store.state.status).toBe("Not connected to a remote backend · /connect-remote <link> connects to one")
  h.controller.dispose()
})

test("/connect-remote without a link opens a concealed entry; the link stays out of the store", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.submit("/connect-remote --transport grpc")
  expect(h.store.state.secretEntry).toEqual({ title: "Relay link", length: 0, hint: secretEntryHint })
  const key = (sequence: string, name = sequence) => ({ name, ctrl: false, meta: false, shift: false, sequence })
  for (const ch of link.slice(0, 10)) h.controller.secretKey(key(ch))
  h.controller.secretKey(key("", "backspace"))
  h.controller.secretKey(key(link[9]!))
  h.controller.secretPaste(link.slice(10))
  expect(h.store.state.secretEntry?.length).toBe(link.length)
  expect(h.visible()).not.toContain("SECRET")
  h.controller.secretKey(key("", "return"))
  await Bun.sleep(0)
  expect(h.store.state.secretEntry).toBeUndefined()
  await Bun.sleep(10)
  expect(h.bridgeCalls).toEqual([{ link, flags: { transport: "grpc" } }])
  expect(h.visible()).not.toContain("SECRET")
  // Esc cancels.
  await h.controller.submit("/connect-remote")
  h.controller.secretKey(key("x"))
  h.controller.secretKey(key("", "escape"))
  expect(h.store.state.secretEntry).toBeUndefined()
  expect(h.bridgeCalls.length).toBe(1)
  h.controller.dispose()
})
