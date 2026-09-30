import { expect, test } from "bun:test"
import { contextFields, contextRows, contextStatusShown, statusLines, type StatusLineSegment } from "../src/state/contextFields"
import { shownServer } from "../src/state/format"
import { createAppStore, type AppStore } from "../src/state/store"

const server = "http://127.0.0.1:8080/"
const text = (lines: StatusLineSegment[][]) => lines.map((line) => line.map((segment) => segment.text).join(" · "))
const rows = (store: AppStore, width = 40) => contextRows(contextFields(store.state, shownServer(store.state, server)), width).map((row) => `${row.label}${row.value}`)

function openedStore() {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", title: "Fix login", agent: "hya-main", workdir: "/home/me/projects/very/long/workspace", model: { providerId: "fake", modelId: "model" }, usage: { output: "12300" } })
  store.setMessages("hysec_1", [{ id: "m", role: "ROLE_USER" }])
  store.setGitBranch("main")
  store.setTodos([{ id: "1", content: "a", status: "TODO_STATUS_COMPLETED" }, { id: "2", content: "b", status: "TODO_STATUS_PENDING" }])
  return store
}

test("the Context box and the top status line carry the same fields", () => {
  const store = openedStore()
  expect(rows(store)).toEqual([
    "Mode     manual",
    "Session  Fix login",
    "Agent    hya-main",
    "Model    fake/model:default",
    "Messages 1",
    "Tokens   12.3k",
    "Dir      …e/projects/very/long/workspace",
    "Branch   main",
    "Todos    1/2",
    "Server   127.0.0.1:8080",
  ])
  expect(text(statusLines(contextFields(store.state, server), 200))).toEqual([
    "mode manual · Fix login · hya-main · model:default · 1 msg · 12.3k tok · …cts/very/long/workspace · ⎇ main · Todos 1/2 · 127.0.0.1:8080",
  ])
})

test("the status line wraps onto a second row, then drops the least essential fields", () => {
  const store = openedStore()
  store.setConnected(false)
  store.setWeb({ url: "http://127.0.0.1:3250/" })
  // Server, then Messages, then Todos drop; the connection state and the WebUI outrank them.
  expect(text(statusLines(contextFields(store.state, server), 80))).toEqual([
    "mode manual · Fix login · hya-main · model:default · 12.3k tok",
    "…cts/very/long/workspace · ⎇ main · WebUI http://127.0.0.1:3250 · reconnecting",
  ])
  // Very narrow: the mode and the session title, one per row.
  expect(text(statusLines(contextFields(store.state, server), 12))).toEqual(["mode manual", "Fix login"])
})

test("the status line is shown exactly when the Context pane is not", () => {
  const store = createAppStore()
  store.setColumns(140)
  expect(contextStatusShown(store.state)).toBe(false)
  store.setSidebar("closed")
  expect(contextStatusShown(store.state)).toBe(true)
  store.setSidebar("auto")
  store.setColumns(109)
  expect(contextStatusShown(store.state)).toBe(true)
  // Below the breakpoint a pin no longer brings the sidebar back.
  store.setSidebar("open")
  expect(contextStatusShown(store.state)).toBe(true)
})

test("without a session: the startup placeholder, then none, the server label, and the WebUI", () => {
  const store = createAppStore()
  expect(rows(store)).toEqual(["Mode     manual", "Session  connecting…", "Server   127.0.0.1:8080"])
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], commands: [] })
  store.setServerLabel("remote: relay.example.com/room")
  store.setWeb({ error: "port 3250 is in use" })
  expect(rows(store)).toEqual(["Mode     manual", "Session  none", "Server   remote: relay.example.com/room", "WebUI    unavailable"])
  expect(contextFields(store.state, "x").find((field) => field.label === "WebUI")).toMatchObject({ short: "WebUI unavailable", tone: "warning" })
})

test("vim, permission mode, occupancy, and connection fields carry their tones", () => {
  const store = openedStore()
  const field = (label: string) => contextFields(store.state, server).find((row) => row.label === label)
  store.setVim(true)
  expect(field("Vim")).toMatchObject({ short: "-- INSERT --", tone: "plain" })
  store.setVimMode("normal", "2d")
  expect(field("Vim")).toMatchObject({ value: "NORMAL 2d", short: "-- NORMAL -- 2d", tone: "accent" })
  expect(text(statusLines(contextFields(store.state, server), 30))[0]).toBe("-- NORMAL -- 2d · mode manual")
  store.setPendingMode("yolo")
  store.openSession({ ...store.state.selected!, permissionMode: "yolo" })
  expect(field("Mode")).toMatchObject({ value: "⚠ yolo", tone: "error" })
  store.applyBootstrap({ models: [{ id: "fake/model", contextLimit: "1000" }] })
  const occupancy = (input: string) => {
    store.applyEvent({ seq: input, session: "hysec_1", tokensRecorded: { message: "m2", model: "fake/model", usage: { input } } })
    return field("Context")
  }
  expect(occupancy("420")).toMatchObject({ value: "42% · 420/1k", short: "ctx 42%", tone: "plain" })
  expect(occupancy("800")!.tone).toBe("warning")
  expect(occupancy("950")!.tone).toBe("error")
  store.setConnected(false)
  expect(field("Backend")).toMatchObject({ short: "reconnecting", tone: "warning" })
  store.setBackendStopped(true)
  expect(field("Backend")).toMatchObject({ short: "backend stopped", tone: "error" })
})

test("the Messages count reflects the merged transcript, and a fork names its source", () => {
  const store = createAppStore()
  store.setSessions([{ id: "hysec_src", agent: "hya-main", workdir: "/w", title: "Parser" }])
  store.openSession({ id: "hysec_1", agent: "hya-main", workdir: "/w", forkedFrom: { session: "hysec_src", messageId: "m" } })
  store.setMessages("hysec_1", [{ id: "m1", role: "ROLE_USER", finish: "FINISH_REASON_STOP" }])
  expect(rows(store)).toContain("Forked   from Parser")
  expect(rows(store)).toContain("Messages 1")
  // A fresh turn's message exists only in the overlay until the next projection read.
  store.applyEvent({ seq: "1", session: "hysec_1", messageStarted: { message: "m2", role: "ROLE_ASSISTANT" } })
  store.flushOverlay()
  expect(rows(store)).toContain("Messages 2")
})
