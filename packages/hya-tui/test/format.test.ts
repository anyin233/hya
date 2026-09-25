import { expect, test } from "bun:test"
import { formatMessage, headerText, mainContent, mainTitle, pendingText, queuedText, sessionListText } from "../src/state/format"
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

test("renders the recorded error of a failed assistant message", () => {
  expect(formatMessage({
    id: "m", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_ERROR",
    error: { code: "provider_error", message: "http status 400: bad request" },
  })).toBe("assistant · error\nerror · provider_error: http status 400: bad request")
})

test("the chat transcript merges streaming text over the projection and lists queued prompts", () => {
  const store = createAppStore()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [] })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setMessages("hysec_1", [{ id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p_u", text: { text: "hi" } }] }])
  store.applyEvent({ seq: "5", session: "hysec_1", messageStarted: { message: "m_a", role: "ROLE_ASSISTANT" } })
  store.applyEvent({ session: "hysec_1", partStarted: { message: "m_a", part: "p_a", kind: "text" } })
  store.applyEvent({ session: "hysec_1", partAppended: { message: "m_a", part: "p_a", textDelta: "Hel" } })
  // Deltas are folded immediately but only shown after a flush (batched rendering).
  expect(mainContent(store.state)).toBe("user · stop\nhi")
  store.flushOverlay()
  expect(mainContent(store.state)).toBe("user · stop\nhi\n\nassistant\nHel")
  store.enqueue("next question", "hysec_1")
  expect(queuedText(store.state)).toBe("user · queued\nnext question")
  store.setView("help")
  expect(queuedText(store.state)).toBe("")
})
