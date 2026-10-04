import { expect, test } from "bun:test"
import { askSessionLabel, compactionText, shownServer, currentModel, modelEffortLabel, modelReference, otherAskNotice, thinkingEffortLabel, webLabel, webNotice, mainContent, mainTitle, pendingLines, sessionTree, truncate, truncateStart } from "../src/state/format"
import { createAppStore } from "../src/state/store"

const server = "http://127.0.0.1:8080/"



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


test("keeps session numbers stable when a running session updates", () => {
  const sessions = [
    { id: "hysec_new", agent: "hya-main", workdir: "/w", timeCreated: "2026-01-02T00:00:00Z", timeUpdated: "2026-01-03T00:00:00Z", busy: true },
    { id: "hysec_old", agent: "hya-main", workdir: "/w", timeCreated: "2026-01-01T00:00:00Z", timeUpdated: "2026-01-01T00:00:00Z" },
  ]
  const before = sessionTree(sessions).map((row) => [row.session.id, row.number])
  sessions[0].busy = false
  sessions[0].timeUpdated = "2026-01-04T00:00:00Z"
  const after = sessionTree([...sessions].reverse()).map((row) => [row.session.id, row.number])
  expect(after).toEqual(before)
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
  expect(askSessionLabel("hysec_2", store.state.sessions, undefined)).toBe("2. Other work")
  expect(askSessionLabel("hysec_1", store.state.sessions, undefined)).toBe("1. hysec_1")
  expect(askSessionLabel("hysec_9", store.state.sessions, undefined)).toBe("hysec_9")
  expect(otherAskNotice(store.state.interactions[0]!, store.state.sessions, undefined)).toBe("Permission needed in 2. Other work · F4 to review")
  expect(otherAskNotice(store.state.interactions[1]!, store.state.sessions, undefined)).toBe("Question in a saved session · F4 to review")
})

test("an ask's session number is the sidebar's: counted in the active Project; another Project's session goes by its title", () => {
  const sessions = [
    { id: "hysec_o", agent: "hya-main", workdir: "/o", projectId: "prj_o", title: "Elsewhere" },
    { id: "hysec_w", agent: "hya-main", workdir: "/w", projectId: "prj_w", title: "Here" },
  ]
  expect(askSessionLabel("hysec_w", sessions, "prj_w")).toBe("1. Here")
  expect(askSessionLabel("hysec_o", sessions, "prj_w")).toBe("Elsewhere")
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
