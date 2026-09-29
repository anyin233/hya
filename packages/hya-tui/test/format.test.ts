import { expect, test } from "bun:test"
import { askSessionLabel, compactionText, contextText, shownServer, currentModel, modelEffortLabel, modelReference, otherAskNotice, thinkingEffortLabel, webLabel, webNotice, headerText, mainContent, mainTitle, pendingLines, sessionListText, sessionTree, statusBarSegments, statusBarText, todosCompactText, truncate, truncateStart } from "../src/state/format"
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
  const selected = { id: "hysec_1", agent: "hya-main", workdir: "/w", model: { providerId: "hya", modelId: "offline" } }
  store.applyCatalog({
    sessions: [selected, { id: "hysec_2", agent: "hya-plan", workdir: "/w", title: "Second", busy: true }],
    interactions: [{ id: "req_1", type: "INTERACTION_TYPE_QUESTION", title: "Pick one" }],
    models: [], workflows: [], providers: [], commands: [],
  })
  store.openSession(selected)
  // The header composes `hya · <session> · <agent> <provider/model:effort> · <server>`.
  expect(headerText(store.state, server)).toContain("hya-main hya/offline:default")
  expect(sessionListText(store.state)).toBe("▸ 1. hysec_1\n   hya-main\n\n  2. Second\n   hya-plan · running")
  expect(sessionListText(store.state, 10)).toBe("▸ 1. hyse…\n   hya-ma…\n\n  2. Seco…\n   hya-pl…")
  expect(pendingLines(store.state)).toEqual(["? Pick one"])
  expect(pendingLines(store.state, 10)).toEqual(["? Pick one"])
  expect(mainContent(store.state)).toBe("No messages yet. Type a prompt below.")
})

test("the model label carries the server-resolved effort right after the model name", () => {
  const withEffort = { id: "hysec_1", agent: "hya-main", workdir: "/w", effectiveEffort: "max", effortSource: "EFFORT_SOURCE_PREFERENCE", model: { providerId: "openai", modelId: "gpt-6-astra" } }
  const explicitNone = { id: "hysec_3", agent: "hya-main", workdir: "/w", effectiveEffort: "none", effortSource: "EFFORT_SOURCE_SUFFIX", model: { providerId: "openai", modelId: "gpt-6-astra", variant: "none" } }
  const withoutEffort = { id: "hysec_2", agent: "hya-main", workdir: "/w", model: { providerId: "openai", modelId: "gpt-6-astra" } }
  expect(modelEffortLabel(withEffort)).toBe("openai/gpt-6-astra:max")
  expect(modelEffortLabel(withEffort, true)).toBe("gpt-6-astra:max")
  // An explicit `#none` is a choice, not the unset default.
  expect(modelEffortLabel(explicitNone, true)).toBe("gpt-6-astra:none")
  expect(modelEffortLabel(withoutEffort, true)).toBe("gpt-6-astra:default")
  expect(modelEffortLabel({ id: "hysec_4", agent: "hya-main", workdir: "/w" })).toBe("")
  expect(thinkingEffortLabel(withEffort)).toBe("max (pref)")
  expect(thinkingEffortLabel(withoutEffort)).toBe("default")
})

test("renders empty panels and per-view titles", () => {
  const store = createAppStore()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], commands: [] })
  expect(headerText(store.state, server)).toBe(`hya · no session · ${server}`)
  expect(sessionListText(store.state)).toBe("No sessions. Type a prompt or /new.")
  expect(mainTitle("api")).toBe("API commands")
  store.setView("models")
  expect(mainContent(store.state)).toBe("No models returned by server.")
})

test("the context box lists the open session, agent, model, message count, directory, and server", () => {
  const store = createAppStore()
  expect(contextText(store.state, server)).toBe("Session  none\nServer   127.0.0.1:8080")
  store.openSession({ id: "hysec_1", agent: "hya-main", workdir: "/home/me/projects/very/long/workspace", model: { providerId: "fake", modelId: "model" } })
  store.setMessages("hysec_1", [{ id: "m", role: "ROLE_USER" }])
  expect(contextText(store.state, server, 30).split("\n")).toEqual([
    "Session  hysec_1",
    "Agent    hya-main",
    "Model    fake/model",
    "Messages 1",
    "Dir      …/very/long/workspace",
    "Server   127.0.0.1:8080",
  ])
})

test("the context box's Messages count reflects the merged transcript, not the raw projection", () => {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "hya-main", workdir: "/w" })
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

test("the status bar keeps the model:effort label visible at 80 columns", () => {
  const fields = { mode: "manual", model: "gpt-6-astra:low", directory: "/home/me/projects/very/long/workspace", branch: "main", todos: "Todos 1/3", connected: true }
  // The segment sits right after the mode, ahead of the drop-from-the-end tail.
  expect(statusBarSegments(fields, 80).slice(0, 2)).toEqual([
    { text: "mode manual", tone: "mode" },
    { text: "gpt-6-astra:low", tone: "muted" },
  ])
  // Narrower: the tail (directory, branch, todos) drops before the label does.
  expect(statusBarText(fields, 30)).toBe("mode manual · gpt-6-astra:low")
  // Tighter than mode + label: the label is dropped whole, then the line clips.
  expect(statusBarText(fields, 24)).toBe("mode manual")
})

test("under bare hya the WebUI address stays on the status bar at 80 columns, ahead of the directory", () => {
  const fields = { mode: "manual", model: "offline:default", directory: "/tmp/hya-tui-launch-BJAycL/work", branch: "", web: { url: "http://127.0.0.1:53855/" }, connected: true }
  // The sidebar (with its WebUI row) is hidden at this width: the bar is the only place left.
  expect(statusBarText(fields, 80)).toBe("mode manual · offline:default · WebUI http://127.0.0.1:53855")
  expect(statusBarText(fields, 120)).toBe("mode manual · offline:default · WebUI http://127.0.0.1:53855 · …-tui-launch-BJAycL/work")
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
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], commands: [] })
  store.openSession({ id: "hysec_1", agent: "hya-main", workdir: "/w" })
  expect(mainContent(store.state)).toBe("No messages yet. Type a prompt below.")
  store.enqueue("next question", "hysec_1")
  expect(mainContent(store.state)).toBe("")
})

test("child sessions nest under their parent in the session list, numbered in that order", () => {
  const store = createAppStore()
  const parent = { id: "hysec_p", agent: "hya-main", workdir: "/w", title: "Parent" }
  const child = { id: "hysec_c", agent: "hya-scout", workdir: "/w", parent: "hysec_p", busy: true }
  const grandchild = { id: "hysec_g", agent: "hya-task", workdir: "/w", parent: "hysec_c" }
  const other = { id: "hysec_o", agent: "hya-plan", workdir: "/w", title: "Other" }
  const orphan = { id: "hysec_x", agent: "hya-task", workdir: "/w", parent: "hysec_gone" }
  // The server lists newest first, so children come before their parent.
  const sessions = [grandchild, child, other, parent, orphan]
  expect(sessionTree(sessions).map((row) => [row.session.id, row.depth])).toEqual([
    ["hysec_o", 0], ["hysec_p", 0], ["hysec_c", 1], ["hysec_g", 2], ["hysec_x", 0],
  ])
  store.applyCatalog({ sessions, interactions: [], models: [], workflows: [], providers: [], commands: [] })
  store.openSession(child)
  expect(sessionListText(store.state)).toBe([
    "  1. Other", "   hya-plan", "",
    "  2. Parent", "   hya-main",
    "▸  ↳ 3. hya-scout · running",
    "     ↳ 4. hya-task", "",
    "  5. hysec_x", "   hya-task",
  ].join("\n"))
})

test("asks of the open session tree are prompts, not pending lines; the sidebar marks sessions that wait", () => {
  const store = createAppStore()
  const parent = { id: "hysec_p", agent: "hya-main", workdir: "/w", title: "Parent" }
  const child = { id: "hysec_c", agent: "hya-task", workdir: "/w", parent: "hysec_p", busy: true }
  const other = { id: "hysec_o", agent: "hya-plan", workdir: "/w", title: "Other" }
  store.applyCatalog({
    sessions: [other, parent, child],
    interactions: [
      { id: "perm_c", session: "hysec_c", type: "INTERACTION_TYPE_PERMISSION", title: "bash ls" },
      { id: "que_o", session: "hysec_o", type: "INTERACTION_TYPE_QUESTION", title: "Why?" },
    ],
    models: [], workflows: [], providers: [], commands: [],
  })
  store.openSession(parent)
  expect(pendingLines(store.state)).toEqual(["? Why? · 1. Other"])
  expect(sessionListText(store.state)).toBe([
    "  1. Other", "   hya-plan · ◌ waiting", "",
    "▸ 2. Parent", "   hya-main",
    "   ↳ 3. hya-task · ◌ waiting",
  ].join("\n"))
})

test("the context box shows the WebUI address when bare hya serves one", () => {
  const store = createAppStore()
  store.setWeb({ url: "http://127.0.0.1:3250/" })
  expect(contextText(store.state, server).split("\n")).toEqual(["Session  none", "Server   127.0.0.1:8080", "WebUI    127.0.0.1:3250"])
  store.setWeb({ error: "port 3250 is in use" })
  expect(contextText(store.state, server).split("\n").at(-1)).toBe("WebUI    unavailable")
})

test("the WebUI notice names the reason and the --port remedy", () => {
  expect(webNotice(undefined)).toBeUndefined()
  expect(webNotice({ url: "http://127.0.0.1:3250/" })).toBeUndefined()
  expect(webNotice({ error: "port 3250 is in use" })).toBe("WebUI unavailable: port 3250 is in use · hya --port <N>")
  expect(webLabel({ url: "http://127.0.0.1:3250/" })).toBe("WebUI http://127.0.0.1:3250")
  expect(webLabel({ error: "x" })).toBe("WebUI unavailable")
})

test("with vim mode on, the status bar starts with the composer's mode (and a pending command)", () => {
  const fields = { mode: "manual", directory: "/w", branch: "main", connected: true }
  expect(statusBarText({ ...fields, vim: { mode: "insert", pending: "" } }, 80)).toBe("-- INSERT -- · mode manual · /w · ⎇ main")
  expect(statusBarText({ ...fields, vim: { mode: "normal", pending: "2d" } }, 80)).toBe("-- NORMAL -- 2d · mode manual · /w · ⎇ main")
  expect(statusBarSegments({ ...fields, vim: { mode: "normal", pending: "" } }, 80)[0]).toEqual({ text: "-- NORMAL --", tone: "accent" })
  expect(statusBarSegments({ ...fields, vim: { mode: "insert", pending: "" } }, 80)[0]).toEqual({ text: "-- INSERT --", tone: "muted" })
  // Narrow: the vim mode and the permission mode stay longest.
  expect(statusBarText({ ...fields, vim: { mode: "normal", pending: "" } }, 30)).toBe("-- NORMAL -- · mode manual")
})

test("pending asks of other sessions name the session they belong to (its /open number and title)", () => {
  const store = createAppStore()
  const selected = { id: "hysec_1", agent: "hya-main", workdir: "/w" }
  store.applyCatalog({
    sessions: [selected, { id: "hysec_2", agent: "hya-main", workdir: "/w", title: "Other work" }],
    interactions: [
      { id: "perm_x", session: "hysec_2", type: "INTERACTION_TYPE_PERMISSION", title: "bash echo x" },
      { id: "que_y", session: "hysec_9", type: "INTERACTION_TYPE_QUESTION", title: "Which one?" },
    ],
    models: [], workflows: [], providers: [], commands: [],
  })
  store.openSession(selected)
  expect(pendingLines(store.state)).toEqual(["! bash echo x · 2. Other work", "? Which one? · saved session"])
  expect(askSessionLabel("hysec_2", store.state.sessions)).toBe("2. Other work")
  expect(askSessionLabel("hysec_1", store.state.sessions)).toBe("1. hysec_1")
  expect(askSessionLabel("hysec_9", store.state.sessions)).toBe("hysec_9")
  expect(otherAskNotice(store.state.interactions[0]!, store.state.sessions)).toBe("Permission needed in 2. Other work · F4 to review")
  expect(otherAskNotice(store.state.interactions[1]!, store.state.sessions)).toBe("Question in a saved session · F4 to review")
})

test("the context box names the session a fork came from", () => {
  const store = createAppStore()
  store.setSessions([{ id: "hysec_src", agent: "hya-main", workdir: "/w", title: "Parser" }])
  store.openSession({ id: "hysec_2", agent: "hya-main", workdir: "/w", forkedFrom: { session: "hysec_src", messageId: "m" } })
  expect(contextText(store.state, server, 30).split("\n")[1]).toBe("Forked   from Parser")
})

test("currentModel looks up the open session's model in the catalog by providerId/modelId; unknown when absent", () => {
  const store = createAppStore()
  expect(currentModel(store.state)).toBeUndefined()
  store.applyBootstrap({ models: [{ id: "openai/gpt-4o", imageInput: false }, { id: "anthropic/claude", imageInput: true }] })
  store.setSelected({ id: "s1", agent: "main", workdir: "/tmp", model: { providerId: "openai", modelId: "gpt-4o" } })
  expect(currentModel(store.state)?.imageInput).toBe(false)
  store.setSelected({ id: "s1", agent: "main", workdir: "/tmp", model: { providerId: "unknown", modelId: "x" } })
  expect(currentModel(store.state)).toBeUndefined()
})

test("a server label replaces the URL in the header and the context box (remote backends)", () => {
  const store = createAppStore()
  expect(shownServer(store.state, server)).toBe(server)
  store.setServerUrl("http://127.0.0.1:6001")
  expect(shownServer(store.state, server)).toBe("http://127.0.0.1:6001")
  store.setServerLabel("remote: relay.example.com/eh7ddx5bksrgcytl7bkai36se4")
  expect(shownServer(store.state, server)).toBe("remote: relay.example.com/eh7ddx5bksrgcytl7bkai36se4")
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], commands: [] })
  expect(headerText(store.state, shownServer(store.state, server))).toBe("hya · no session · remote: relay.example.com/eh7ddx5bksrgcytl7bkai36se4")
  expect(contextText(store.state, shownServer(store.state, server), 70)).toBe("Session  none\nServer   remote: relay.example.com/eh7ddx5bksrgcytl7bkai36se4")
  store.setServerLabel(undefined)
  expect(shownServer(store.state, server)).toBe("http://127.0.0.1:6001")
})
