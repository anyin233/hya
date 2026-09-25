import { expect, test } from "bun:test"
import { compactionText, contextText, headerText, mainContent, mainTitle, pendingLines, sessionListText, sessionTree, statusBarText, todosCompactText, truncate, truncateStart } from "../src/state/format"
import { createAppStore } from "../src/state/store"

const server = "http://127.0.0.1:8080/"

test("keeps the startup placeholders until the first data arrives", () => {
  const store = createAppStore()
  expect(headerText(store.state, server)).toBe("hya · connecting…")
  expect(sessionListText(store.state)).toBe("Loading…")
  expect(pendingLines(store.state)).toEqual([])
  expect(mainContent(store.state)).toBe("")
})

test("renders the header, the sidebar session list, and pending lines", () => {
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
  expect(sessionListText(store.state, 10)).toBe("▸ 1. hyse…\n   build\n\n  2. Seco…\n   plan ·…")
  expect(pendingLines(store.state)).toEqual(["? Pick one · req_1"])
  expect(pendingLines(store.state, 10)).toEqual(["? Pick on…"])
  expect(mainContent(store.state)).toBe("No messages yet. Type a prompt below.")
})

test("renders empty panels and per-view titles", () => {
  const store = createAppStore()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [] })
  expect(headerText(store.state, server)).toBe(`hya · no session · ${server}`)
  expect(sessionListText(store.state)).toBe("No sessions. Type a prompt or /new.")
  expect(mainTitle("keys")).toBe("Saved provider keys")
  expect(mainTitle("api")).toBe("API commands")
  store.setView("keys")
  expect(mainContent(store.state)).toBe("No providers or saved keys. Use /key set <provider> to add one.")
  store.setView("models")
  expect(mainContent(store.state)).toBe("No models returned by server.")
})

test("the context box lists the open session, agent, model, message count, directory, and server", () => {
  const store = createAppStore()
  expect(contextText(store.state, server)).toBe("Session  none\nServer   127.0.0.1:8080")
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/home/me/projects/very/long/workspace", model: { providerId: "fake", modelId: "model" } })
  store.setMessages("hysec_1", [{ id: "m", role: "ROLE_USER" }])
  expect(contextText(store.state, server, 30).split("\n")).toEqual([
    "Session  hysec_1",
    "Agent    build",
    "Model    fake/model",
    "Messages 1",
    "Dir      …/very/long/workspace",
    "Server   127.0.0.1:8080",
  ])
})

test("the context box's Messages count reflects the merged transcript, not the raw projection", () => {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setMessages("hysec_1", [{ id: "m1", role: "ROLE_USER", finish: "FINISH_REASON_STOP" }])
  expect(contextText(store.state, server, 30).split("\n")[3]).toBe("Messages 1")
  // A fresh turn's message exists only in the overlay until the next projection read.
  store.applyEvent({ seq: "1", session: "hysec_1", messageStarted: { message: "m2", role: "ROLE_ASSISTANT" } })
  store.flushOverlay()
  expect(contextText(store.state, server, 30).split("\n")[3]).toBe("Messages 2")
})

test("the status bar shows mode, directory, branch, todos, and connection state, truncating gracefully", () => {
  const fields = { mode: "manual", directory: "/home/me/projects/very/long/workspace", branch: "main", todos: "Todos 1/3", connected: true }
  expect(statusBarText(fields, 80)).toBe("mode manual · …cts/very/long/workspace · ⎇ main · Todos 1/3")
  expect(statusBarText({ ...fields, connected: false }, 80)).toBe("mode manual · …cts/very/long/workspace · ⎇ main · Todos 1/3 · reconnecting")
  // No branch, no todos: those segments are omitted, not shown empty.
  expect(statusBarText({ mode: "yolo", directory: "", branch: "", connected: true }, 80)).toBe("mode yolo")
  // Too narrow: the least essential segments drop first, then the whole line clips.
  expect(statusBarText(fields, 20)).toBe("mode manual")
})

test("a compact todo count is `completed/total`, or undefined with no todos", () => {
  expect(todosCompactText([])).toBeUndefined()
  expect(todosCompactText([
    { id: "1", content: "a", status: "TODO_STATUS_COMPLETED" },
    { id: "2", content: "b", status: "TODO_STATUS_PENDING" },
    { id: "3", content: "c", status: "TODO_STATUS_IN_PROGRESS" },
  ])).toBe("Todos 1/3")
})

test("a compaction divider reads the strategy; the payload carries no message count", () => {
  expect(compactionText({ strategy: "shake", untilSeq: "42" })).toBe("── context compacted · shake ──")
  expect(compactionText({})).toBe("── context compacted · unknown ──")
})

test("truncates from either end", () => {
  expect(truncate("abcdef", 4)).toBe("abc…")
  expect(truncate("abc", 4)).toBe("abc")
  expect(truncate("abc")).toBe("abc")
  expect(truncateStart("/a/b/c/d", 5)).toBe("…/c/d")
})

test("the chat view's text is only the empty-state hint; messages render per component", () => {
  const store = createAppStore()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [] })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  expect(mainContent(store.state)).toBe("No messages yet. Type a prompt below.")
  store.enqueue("next question", "hysec_1")
  expect(mainContent(store.state)).toBe("")
})

test("child sessions nest under their parent in the session list, numbered in that order", () => {
  const store = createAppStore()
  const parent = { id: "hysec_p", agent: "build", workdir: "/w", title: "Parent" }
  const child = { id: "hysec_c", agent: "scout", workdir: "/w", parent: "hysec_p", busy: true }
  const grandchild = { id: "hysec_g", agent: "general", workdir: "/w", parent: "hysec_c" }
  const other = { id: "hysec_o", agent: "plan", workdir: "/w", title: "Other" }
  const orphan = { id: "hysec_x", agent: "general", workdir: "/w", parent: "hysec_gone" }
  // The server lists newest first, so children come before their parent.
  const sessions = [grandchild, child, other, parent, orphan]
  expect(sessionTree(sessions).map((row) => [row.session.id, row.depth])).toEqual([
    ["hysec_o", 0], ["hysec_p", 0], ["hysec_c", 1], ["hysec_g", 2], ["hysec_x", 0],
  ])
  store.applyCatalog({ sessions, interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [] })
  store.openSession(child)
  expect(sessionListText(store.state)).toBe([
    "  1. Other", "   plan", "",
    "  2. Parent", "   build",
    "▸  ↳ 3. scout · running",
    "     ↳ 4. general", "",
    "  5. hysec_x", "   general",
  ].join("\n"))
})

test("asks of the open session tree are prompts, not pending lines; the sidebar marks sessions that wait", () => {
  const store = createAppStore()
  const parent = { id: "hysec_p", agent: "build", workdir: "/w", title: "Parent" }
  const child = { id: "hysec_c", agent: "general", workdir: "/w", parent: "hysec_p", busy: true }
  const other = { id: "hysec_o", agent: "plan", workdir: "/w", title: "Other" }
  store.applyCatalog({
    sessions: [other, parent, child],
    interactions: [
      { id: "perm_c", session: "hysec_c", type: "INTERACTION_TYPE_PERMISSION", title: "bash ls" },
      { id: "que_o", session: "hysec_o", type: "INTERACTION_TYPE_QUESTION", title: "Why?" },
    ],
    models: [], workflows: [], providers: [], savedKeys: [], commands: [],
  })
  store.openSession(parent)
  expect(pendingLines(store.state)).toEqual(["? Why? · que_o"])
  expect(sessionListText(store.state)).toBe([
    "  1. Other", "   plan · ◌ waiting", "",
    "▸ 2. Parent", "   build",
    "   ↳ 3. general · ◌ waiting",
  ].join("\n"))
})
