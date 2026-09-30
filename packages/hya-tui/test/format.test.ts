import { expect, test } from "bun:test"
import { askSessionLabel, compactionText, shownServer, currentModel, modelEffortLabel, modelReference, otherAskNotice, thinkingEffortLabel, webLabel, webNotice, mainContent, mainTitle, pendingLines, sessionListText, sessionTree, truncate, truncateStart } from "../src/state/format"
import { createAppStore } from "../src/state/store"

const server = "http://127.0.0.1:8080/"

test("keeps the startup placeholders until the first data arrives", () => {
  const store = createAppStore()
  expect(sessionListText(store.state)).toBe("Loading…")
  expect(pendingLines(store.state)).toEqual([])
  expect(mainContent(store.state)).toBe("")
})

test("renders the sidebar session list and pending lines", () => {
  const store = createAppStore()
  const selected = { id: "hysec_1", agent: "hya-main", workdir: "/w", model: { providerId: "hya", modelId: "offline" } }
  store.applyCatalog({
    sessions: [selected, { id: "hysec_2", agent: "hya-plan", workdir: "/w", title: "Second", busy: true }],
    interactions: [{ id: "req_1", type: "INTERACTION_TYPE_QUESTION", title: "Pick one" }],
    models: [], workflows: [], providers: [], commands: [],
  })
  store.openSession(selected)
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
  expect(sessionListText(store.state)).toBe("No sessions. Type a prompt or /new.")
  expect(mainTitle("api")).toBe("API commands")
  store.setView("models")
  expect(mainContent(store.state)).toBe("No models returned by server.")
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

test("the WebUI notice names the reason and the --port remedy", () => {
  expect(webNotice(undefined)).toBeUndefined()
  expect(webNotice({ url: "http://127.0.0.1:3250/" })).toBeUndefined()
  expect(webNotice({ error: "port 3250 is in use" })).toBe("WebUI unavailable: port 3250 is in use · hya --port <N>")
  expect(webLabel({ url: "http://127.0.0.1:3250/" })).toBe("WebUI http://127.0.0.1:3250")
  expect(webLabel({ error: "x" })).toBe("WebUI unavailable")
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

test("currentModel looks up the open session's model in the catalog by providerId/modelId; unknown when absent", () => {
  const store = createAppStore()
  expect(currentModel(store.state)).toBeUndefined()
  store.applyBootstrap({ models: [{ id: "openai/gpt-4o", imageInput: false }, { id: "anthropic/claude", imageInput: true }] })
  store.setSelected({ id: "s1", agent: "main", workdir: "/tmp", model: { providerId: "openai", modelId: "gpt-4o" } })
  expect(currentModel(store.state)?.imageInput).toBe(false)
  store.setSelected({ id: "s1", agent: "main", workdir: "/tmp", model: { providerId: "unknown", modelId: "x" } })
  expect(currentModel(store.state)).toBeUndefined()
})

test("a server label replaces the URL wherever the server is shown (remote backends)", () => {
  const store = createAppStore()
  expect(shownServer(store.state, server)).toBe(server)
  store.setServerUrl("http://127.0.0.1:6001")
  expect(shownServer(store.state, server)).toBe("http://127.0.0.1:6001")
  store.setServerLabel("remote: relay.example.com/eh7ddx5bksrgcytl7bkai36se4")
  expect(shownServer(store.state, server)).toBe("remote: relay.example.com/eh7ddx5bksrgcytl7bkai36se4")
  store.setServerLabel(undefined)
  expect(shownServer(store.state, server)).toBe("http://127.0.0.1:6001")
})
