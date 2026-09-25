import { expect, test } from "bun:test"
import { formatMessage, headerText, mainContent, mainTitle, pendingText, sessionListText } from "../src/state/format"
import { createAppStore } from "../src/state/store"

const server = "http://127.0.0.1:8080/"

test("keeps the startup placeholders until the first data arrives", () => {
  const store = createAppStore()
  expect(headerText(store.state, server)).toBe("hya · connecting…")
  expect(sessionListText(store.state)).toBe("Loading…")
  expect(pendingText(store.state)).toBe("")
  expect(mainContent(store.state)).toBe("")
})

test("renders the header, session list, and pending list like the original panels", () => {
  const store = createAppStore()
  const selected = { id: "hysec_1", agent: "build", workdir: "/w", model: { providerId: "hya", modelId: "offline" } }
  store.applyCatalog({
    sessions: [selected, { id: "hysec_2", agent: "plan", workdir: "/w", title: "Second", busy: true }],
    interactions: [{ id: "req_1", type: "INTERACTION_TYPE_QUESTION", title: "Pick one" }],
    models: [], workflows: [], providers: [], savedKeys: [], commands: [],
  })
  store.openSession(selected)
  expect(headerText(store.state, server)).toBe(`hya · hysec_1 · build hya/offline · ${server}`)
  expect(sessionListText(store.state)).toBe("▸ 1. hysec_1\n   build\n\n  2. Second\n   plan · running")
  expect(pendingText(store.state)).toBe("? Pick one\nreq_1")
  expect(mainContent(store.state)).toBe("No messages yet. Type a prompt below.")
})

test("renders empty panels and per-view titles", () => {
  const store = createAppStore()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [] })
  expect(headerText(store.state, server)).toBe(`hya · no session · ${server}`)
  expect(sessionListText(store.state)).toBe("No sessions. Type a prompt or /new.")
  expect(pendingText(store.state)).toBe("No pending requests")
  expect(mainTitle("keys")).toBe("Saved provider keys")
  expect(mainTitle("api")).toBe("API commands")
  store.setView("keys")
  expect(mainContent(store.state)).toBe("No providers or saved keys. Use /key set <provider> to add one.")
  store.setView("models")
  expect(mainContent(store.state)).toBe("No models returned by server.")
})

test("formats a transcript message with role, finish reason, and parts", () => {
  expect(formatMessage({
    id: "m", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP",
    parts: [{ id: "p1", text: { text: "hi" } }, { id: "p2", toolCall: { tool: "bash", state: "done" } }],
  })).toBe("assistant · stop\nhi\n↳ bash  done")
})
