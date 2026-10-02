import { expect, test } from "bun:test"
import { contextFields, contextRows, contextStatus } from "../src/state/contextFields"
import { shownServer } from "../src/state/format"
import { createAppStore, type AppStore } from "../src/state/store"

const server = "http://127.0.0.1:8080/"
const rows = (store: AppStore, width = 40) => contextRows(contextFields(store.state, shownServer(store.state, server)), width).map((row) => `${row.label}${row.value}`)

function openedStore() {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", title: "Fix login", agent: "hya-main", workdir: "/home/me/projects/very/long/workspace", model: { providerId: "fake", modelId: "model" }, usage: { output: "12300" } })
  store.setMessages("hysec_1", [{ id: "m", role: "ROLE_USER" }])
  store.setGitBranch("main")
  store.setTodos([{ id: "1", content: "a", status: "TODO_STATUS_COMPLETED" }, { id: "2", content: "b", status: "TODO_STATUS_PENDING" }])
  return store
}



test("vim, permission mode, occupancy, and connection fields carry their tones", () => {
  const store = openedStore()
  const field = (label: string) => contextFields(store.state, server).find((row) => row.label === label)
  store.setVim(true)
  expect(field("Vim")).toMatchObject({ short: "-- INSERT --", tone: "plain" })
  store.setVimMode("normal", "2d")
  expect(field("Vim")).toMatchObject({ value: "NORMAL 2d", short: "-- NORMAL -- 2d", tone: "accent" })
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
