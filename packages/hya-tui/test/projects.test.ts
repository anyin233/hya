import { expect, test } from "bun:test"
import type { HyaClient, ProjectInfo, SessionInfo, SessionPlacement, StreamFrame } from "../src/client"
import { createController } from "../src/app/controller"
import { activeProject, newestTopLevelSession, noProjectStatus, pathInside, projectBusy, projectScope, sessionPlacement } from "../src/state/projects"
import { createAppStore } from "../src/state/store"

const work: ProjectInfo = { id: "prj_work", name: "work", roots: ["/work", "/docs"], busy: false }
const other: ProjectInfo = { id: "prj_other", name: "other", roots: ["/other"], busy: true }

test("pathInside matches the root itself and paths below it, not siblings sharing a prefix", () => {
  expect(pathInside("/work", "/work")).toBe(true)
  expect(pathInside("/work/sub/x", "/work")).toBe(true)
  expect(pathInside("/work/sub", "/work/")).toBe(true)
  expect(pathInside("/workshop", "/work")).toBe(false)
  expect(pathInside("/elsewhere", "/work")).toBe(false)
  expect(pathInside("/anything", "/")).toBe(true)
})

test("projectScope: --dir when a local start lies inside the Project, else its primary root", () => {
  expect(projectScope(work, "/docs/api", false)).toBe("/docs/api")
  expect(projectScope(work, "/elsewhere", false)).toBe("/work")
  expect(projectScope(work, "/work/sub", true)).toBe("/work")
})

test("sessionPlacement: workdir = --dir only for a local start inside the active Project; temporary; refused remotely without a Project", () => {
  expect(sessionPlacement({ project: work, directory: "/work/sub", remote: false })).toEqual({ projectId: "prj_work", workdir: "/work/sub" })
  expect(sessionPlacement({ project: work, directory: "/elsewhere", remote: false })).toEqual({ projectId: "prj_work" })
  expect(sessionPlacement({ project: work, directory: "/work/sub", remote: true })).toEqual({ projectId: "prj_work" })
  expect(sessionPlacement({ project: undefined, directory: "/work", remote: false })).toEqual({ workdir: "/work" })
  expect(sessionPlacement({ project: undefined, directory: "/work", remote: true })).toBeUndefined()
  expect(sessionPlacement({ project: work, directory: "/work", remote: false, temporary: true })).toEqual({ temporary: true })
  expect(sessionPlacement({ project: undefined, directory: "/work", remote: true, temporary: true })).toEqual({ temporary: true })
})

test("newestTopLevelSession picks the Project's most recently updated root session", () => {
  const sessions: SessionInfo[] = [
    { id: "a", agent: "b", workdir: "/work", projectId: "prj_work", timeUpdated: "2026-09-25T10:00:00Z" },
    { id: "kid", agent: "b", workdir: "/work", projectId: "prj_work", parent: "a", timeUpdated: "2026-09-25T12:00:00Z" },
    { id: "c", agent: "b", workdir: "/docs", projectId: "prj_work", timeUpdated: "2026-09-25T11:00:00Z" },
    { id: "o", agent: "b", workdir: "/other", projectId: "prj_other", timeUpdated: "2026-09-25T13:00:00Z" },
  ]
  expect(newestTopLevelSession(sessions, "prj_work")?.id).toBe("c")
  expect(newestTopLevelSession(sessions, "prj_none")).toBeUndefined()
})

test("the store keeps the Project list, the active Project, and per-Project busy flags", () => {
  const store = createAppStore()
  expect(store.state.projects).toEqual([])
  expect(store.state.activeProjectId).toBeUndefined()
  store.setProjects([work, other])
  store.setActiveProject("prj_other")
  expect(activeProject(store.state)?.name).toBe("other")
  expect(projectBusy(store.state, "prj_other")).toBe(true)
  expect(projectBusy(store.state, "prj_work")).toBe(false)
  store.setActiveProject(undefined)
  expect(activeProject(store.state)).toBeUndefined()
})

/** A fake server behind the controller: records calls; streams stay open until aborted. */
function harness(options: { directory?: string; remote?: boolean; sessions?: SessionInfo[]; projects?: ProjectInfo[]; startup?: { continue: boolean; session?: string } } = {}) {
  const store = createAppStore()
  const calls: Array<[string, ...unknown[]]> = []
  const sessions = [...(options.sessions ?? [])]
  let projects = [...(options.projects ?? [work, other])]
  let directory = options.directory ?? "/work/sub"
  let globalFrame: ((frame: StreamFrame) => void | Promise<void>) | undefined
  let created = 0
  const untilAbort = (signal: AbortSignal) => new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }))
  const client = {
    get directory() { return directory },
    setDirectory(next: string) { directory = next; calls.push(["setDirectory", next]) },
    bootstrap: async () => ({ location: { version: "test" }, agents: [{ name: "build" }], models: [{ id: "hya/echo", providerId: "hya", modelId: "echo" }] }),
    ensureProjectForPath: async (path: string) => {
      calls.push(["ensureProjectForPath", path])
      return { project: work, created: false }
    },
    listProjects: async () => { calls.push(["listProjects"]); return projects },
    getProject: async (id: string) => projects.find((row) => row.id === id)!,
    listSessions: async (filter?: { projectId?: string }) => sessions.filter((row) => !filter?.projectId || row.projectId === filter.projectId),
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
    request: async (_method: string, path: string) => {
      const id = decodeURIComponent(path.split("/").at(-1) ?? "")
      const row = sessions.find((session) => session.id === id)
      if (!row) throw new Error(`not found ${id}`)
      return row
    },
    createSession: async (agent: string, model: string, placement: SessionPlacement) => {
      calls.push(["createSession", placement])
      const temporary = "temporary" in placement && placement.temporary
      const projectId = temporary ? undefined : ("projectId" in placement && placement.projectId) || "prj_work"
      const workdir = temporary ? "/cache/scratch" : ("workdir" in placement && placement.workdir) || projects.find((row) => row.id === projectId)?.roots[0] || "/work"
      const session: SessionInfo = { id: `new_${++created}`, agent, workdir, model: { providerId: "hya", modelId: "echo" }, ...(projectId ? { projectId } : {}), kind: temporary ? "SESSION_KIND_TEMPORARY" : "SESSION_KIND_PROJECT" }
      sessions.unshift(session)
      return session
    },
    streamGlobal: (onFrame: (frame: StreamFrame) => void | Promise<void>, signal: AbortSignal, onOpen?: () => void | Promise<void>) => {
      globalFrame = onFrame
      void onOpen?.()
      return untilAbort(signal)
    },
    streamSession: async (session: string, _since: string, _onFrame: unknown, signal: AbortSignal, onOpen?: () => void | Promise<void>) => {
      calls.push(["streamSession", session])
      await onOpen?.()
      return untilAbort(signal)
    },
  }
  const controller = createController({
    client: client as unknown as HyaClient,
    store,
    directory: options.directory ?? "/work/sub",
    ...(options.remote ? { remote: true } : {}),
    ...(options.startup ? { startup: options.startup } : {}),
  })
  return {
    store, calls, controller, sessions,
    setProjects(next: ProjectInfo[]) { projects = next },
    get directory() { return directory },
    global: (frame: StreamFrame) => globalFrame!(frame),
    named: (name: string) => calls.filter((call) => call[0] === name),
  }
}

test("a local start ensures the Project of --dir and makes it active; the Project list is loaded", async () => {
  const h = harness()
  await h.controller.start()
  expect(h.named("ensureProjectForPath")).toEqual([["ensureProjectForPath", "/work/sub"]])
  expect(h.store.state.activeProjectId).toBe("prj_work")
  expect(h.store.state.projects.map((row) => row.id)).toEqual(["prj_work", "prj_other"])
  expect(h.store.state.remote).toBe(false)
  h.controller.dispose()
})

test("a new session in the active Project works in --dir when --dir lies inside it, else in the primary root", async () => {
  const inside = harness({ directory: "/docs/api" })
  await inside.controller.start()
  await inside.controller.newSession()
  expect(inside.named("createSession")).toEqual([["createSession", { projectId: "prj_work", workdir: "/docs/api" }]])
  expect(inside.store.state.selected?.id).toBe("new_1")
  inside.controller.dispose()

  const outside = harness({ directory: "/work/sub" })
  await outside.controller.start()
  await outside.controller.switchProject("prj_other")
  expect(outside.named("createSession")).toEqual([["createSession", { projectId: "prj_other" }]])
  outside.controller.dispose()
})

test("newTemporarySession creates a SESSION_KIND_TEMPORARY session", async () => {
  const h = harness()
  await h.controller.start()
  await h.controller.newTemporarySession()
  expect(h.named("createSession")).toEqual([["createSession", { temporary: true }]])
  expect(h.store.state.selected?.kind).toBe("SESSION_KIND_TEMPORARY")
  // The active Project stays: the next /new goes back to it.
  expect(h.store.state.activeProjectId).toBe("prj_work")
  h.controller.dispose()
})

test("switchProject sets the active Project and scope, opens its newest root session, and restarts the stream", async () => {
  const h = harness({
    sessions: [
      { id: "w1", agent: "build", workdir: "/work", projectId: "prj_work", timeUpdated: "2026-09-25T10:00:00Z" },
      { id: "o1", agent: "build", workdir: "/other", projectId: "prj_other", timeUpdated: "2026-09-25T09:00:00Z" },
      { id: "o2", agent: "build", workdir: "/other/x", projectId: "prj_other", timeUpdated: "2026-09-25T11:00:00Z" },
      { id: "o2-kid", agent: "explore", workdir: "/other/x", projectId: "prj_other", parent: "o2", timeUpdated: "2026-09-25T12:00:00Z" },
    ],
  })
  await h.controller.start()
  await h.controller.switchProject("prj_other")
  expect(h.store.state.activeProjectId).toBe("prj_other")
  expect(h.directory).toBe("/other")
  expect(h.store.state.selected?.id).toBe("o2")
  expect(h.named("streamSession").at(-1)).toEqual(["streamSession", "o2"])
  expect(h.named("createSession")).toEqual([])
  // Back to the Project that contains --dir: the scope is --dir again.
  await h.controller.switchProject("prj_work")
  expect(h.directory).toBe("/work/sub")
  expect(h.store.state.selected?.id).toBe("w1")
  h.controller.dispose()
})

test("--continue opens the newest root session of the ensured Project, whatever its workdir", async () => {
  const h = harness({
    startup: { continue: true },
    sessions: [
      { id: "old", agent: "build", workdir: "/work/sub", projectId: "prj_work", timeUpdated: "2026-09-25T10:00:00Z" },
      { id: "newer", agent: "build", workdir: "/docs", projectId: "prj_work", timeUpdated: "2026-09-25T11:00:00Z" },
      { id: "foreign", agent: "build", workdir: "/work/sub", projectId: "prj_other", timeUpdated: "2026-09-25T12:00:00Z" },
    ],
  })
  await h.controller.start()
  expect(h.store.state.selected?.id).toBe("newer")
  h.controller.dispose()
})

test("a projectsUpdated frame on the global stream re-reads the Project list (debounced)", async () => {
  const h = harness()
  await h.controller.start()
  const before = h.named("listProjects").length
  h.setProjects([{ ...work, busy: true }, other])
  await h.global({ event: { projectsUpdated: {} } })
  await h.global({ event: { projectsUpdated: {} } })
  await h.global({ event: { projectsUpdated: {} } })
  await Bun.sleep(200)
  expect(h.named("listProjects").length).toBe(before + 1)
  expect(projectBusy(h.store.state, "prj_work")).toBe(true)
  h.controller.dispose()
})

test("--remote starts without ensuring a Project and refuses a new session until one is chosen", async () => {
  const h = harness({ remote: true })
  await h.controller.start()
  expect(h.named("ensureProjectForPath")).toEqual([])
  expect(h.store.state.activeProjectId).toBeUndefined()
  expect(h.store.state.remote).toBe(true)
  expect(h.store.state.projects.length).toBe(2)
  await h.controller.submit("hello")
  expect(h.named("createSession")).toEqual([])
  expect(h.store.state.status).toBe(noProjectStatus)
  await expect(h.controller.newSession()).rejects.toThrow()
  expect(h.store.state.status).toBe(noProjectStatus)
  // A temporary session needs no Project.
  await h.controller.newTemporarySession()
  expect(h.named("createSession")).toEqual([["createSession", { temporary: true }]])
  // Choosing a Project remotely scopes to its primary root.
  await h.controller.switchProject("prj_work")
  expect(h.directory).toBe("/work")
  expect(h.named("createSession").at(-1)).toEqual(["createSession", { projectId: "prj_work" }])
  h.controller.dispose()
})
