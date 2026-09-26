import { expect, test } from "bun:test"
import { compactionText, contextUsage, formatTokens, sessionTokens, statusBarSegments, statusBarText, strategyText } from "../src/state/format"
import { transcriptViews } from "../src/state/messages"
import { askFrameRoute } from "../src/state/prompts"
import { createAppStore } from "../src/state/store"

const open = (extra: Record<string, unknown> = {}) => {
  const store = createAppStore()
  store.applyCatalog({
    sessions: [], interactions: [], workflows: [], providers: [], commands: [],
    models: [{ id: "fake/model", contextLimit: "1000" }, { id: "fake/nolimit" }],
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w", ...extra })
  return store
}

test("token counts (uint64 strings) format compactly", () => {
  expect(formatTokens("0")).toBe("0")
  expect(formatTokens("950")).toBe("950")
  expect(formatTokens("12345")).toBe("12.3k")
  expect(formatTokens("123456")).toBe("123k")
  expect(formatTokens("1234567")).toBe("1.2M")
  expect(formatTokens("18446744073709551615")).toMatch(/M$/)
})

test("session tokens sum input, cache, and output of SessionInfo.usage; unknown is undefined", () => {
  expect(sessionTokens(undefined)).toBeUndefined()
  expect(sessionTokens({})).toBeUndefined()
  expect(sessionTokens({ input: "100", output: "20", cacheRead: "1000", cacheWrite: "5", reasoning: "10" })).toBe(1125)
})

test("context % comes from the newest assistant message's roundUsage against its model's limit", () => {
  const store = open()
  expect(contextUsage(store.state)).toBeUndefined()
  store.setMessages("hysec_1", [
    { id: "m1", role: "ROLE_ASSISTANT", model: "fake/model", finish: "FINISH_REASON_STOP", roundUsage: { input: "100", cacheRead: "200", cacheWrite: "0", output: "999" } },
    { id: "m2", role: "ROLE_USER", finish: "FINISH_REASON_STOP" },
    { id: "m3", role: "ROLE_ASSISTANT", model: "fake/model", finish: "FINISH_REASON_STOP", roundUsage: { input: "300", cacheRead: "100", cacheWrite: "20" } },
    { id: "m4", role: "ROLE_ASSISTANT", model: "fake/model" },
  ])
  expect(contextUsage(store.state)).toEqual({ percent: 42, tokens: 420, limit: 1000 })
})

test("context % is hidden when the model has no known limit or no usage", () => {
  const store = open()
  store.setMessages("hysec_1", [{ id: "m1", role: "ROLE_ASSISTANT", model: "fake/nolimit", roundUsage: { input: "300" } }])
  expect(contextUsage(store.state)).toBeUndefined()
  store.setMessages("hysec_1", [{ id: "m1", role: "ROLE_ASSISTANT", model: "fake/model", roundUsage: { input: "0" } }])
  expect(contextUsage(store.state)).toBeUndefined()
})

test("live: the newest tokensRecorded with a message wins; side calls (empty message) do not count", () => {
  const store = open()
  store.setMessages("hysec_1", [{ id: "m1", role: "ROLE_ASSISTANT", model: "fake/model", roundUsage: { input: "100" } }])
  store.applyEvent({ seq: "5", session: "hysec_1", tokensRecorded: { message: "m2", model: "fake/model", usage: { input: "800", cacheRead: "150" } } })
  expect(contextUsage(store.state)).toEqual({ percent: 95, tokens: 950, limit: 1000 })
  store.applyEvent({ seq: "6", session: "hysec_1", tokensRecorded: { message: "", model: "fake/model", usage: { input: "10" } } })
  expect(contextUsage(store.state)?.tokens).toBe(950)
  // A session switch forgets the live round.
  store.openSession({ id: "hysec_2", agent: "build", workdir: "/w" })
  expect(contextUsage(store.state)).toBeUndefined()
})

test("the status bar shows ctx % toned by level and the session token total, hidden when unknown", () => {
  const base = { mode: "manual", directory: "/w", branch: "", connected: true }
  expect(statusBarText({ ...base, context: 42, tokens: "12.3k tok" }, 80)).toBe("mode manual · ctx 42% · 12.3k tok · /w")
  expect(statusBarText(base, 80)).toBe("mode manual · /w")
  const tone = (context: number) => statusBarSegments({ ...base, context }, 80).find((segment) => segment.text.startsWith("ctx"))?.tone
  expect(tone(42)).toBe("muted")
  expect(tone(80)).toBe("warning")
  expect(tone(95)).toBe("error")
})

test("todoUpdated frames replace the open session's todo list", () => {
  const store = open()
  store.setTodos([{ id: "1", content: "old", status: "TODO_STATUS_PENDING" }])
  store.applyEvent({ seq: "7", session: "hysec_1", todoUpdated: { items: [{ id: "1", content: "write tests", status: "TODO_STATUS_IN_PROGRESS" }] } })
  expect(store.state.todos).toEqual([{ id: "1", content: "write tests", status: "TODO_STATUS_IN_PROGRESS" }])
  store.applyEvent({ seq: "8", session: "hysec_1", todoUpdated: {} })
  expect(store.state.todos).toEqual([])
})

test("the compaction divider reads the folded count, manual flag, and human strategy name", () => {
  expect(compactionText({ strategy: "local_summarizer", foldedCount: 12, manual: true })).toBe("── context compacted · 12 messages · manual · local summary ──")
  expect(compactionText({ strategy: "native", foldedCount: 1 })).toBe("── context compacted · 1 message · native ──")
  expect(compactionText({ strategy: "snap_compact" })).toBe("── context compacted · snapshot ──")
  expect(compactionText({ strategy: "handoff", manual: true })).toBe("── context compacted · manual · handoff ──")
})

test("strategyText names every known strategy in words; an unknown or missing one falls back", () => {
  expect(strategyText("native")).toBe("native")
  expect(strategyText("local_summarizer")).toBe("local summary")
  expect(strategyText("snap_compact")).toBe("snapshot")
  expect(strategyText("handoff")).toBe("handoff")
  expect(strategyText("shake")).toBe("shake")
  expect(strategyText(undefined)).toBe("unknown")
})

test("a compaction divider sits right before its summary message (compactionApplied.message)", () => {
  const store = open()
  const text = (id: string, role: string, value: string) => ({ id, role, finish: "FINISH_REASON_STOP", parts: [{ id: `p-${id}`, text: { text: value } }] })
  store.setMessages("hysec_1", [text("m1", "ROLE_USER", "hi"), text("m2", "ROLE_ASSISTANT", "hello")])
  store.applyEvent({ seq: "9", session: "hysec_1", compactionApplied: { untilSeq: "9", strategy: "local_summarizer", message: "sum", foldedCount: 2, manual: true } })
  // The summary message is not read yet: the divider goes at the end.
  expect(transcriptViews(store.state).map((view) => view.role)).toEqual(["user", "assistant", "divider"])
  // It is now: the divider moves right before it, even with later messages after it.
  store.setMessages("hysec_1", [text("m1", "ROLE_USER", "hi"), text("m2", "ROLE_ASSISTANT", "hello"), text("sum", "ROLE_SYSTEM", "Summary: greeted"), text("m5", "ROLE_USER", "next")])
  const views = transcriptViews(store.state)
  expect(views.map((view) => view.id)).toEqual(["m1", "m2", "divider-9", "sum", "m5"])
  expect(views[2]!.blocks[0]).toMatchObject({ text: "── context compacted · 2 messages · manual · local summary ──" })
})

test("descendant ask frames on the open session's stream are routed to the prompt list, other descendant frames are dropped", () => {
  const ask = { id: "q1", session: "hysec_child", type: "INTERACTION_TYPE_QUESTION", title: "Which?", options: ["a", "b"] }
  expect(askFrameRoute({ session: "hysec_1", messageStarted: { message: "m" } }, "hysec_1")).toBe("own")
  expect(askFrameRoute({ session: "hysec_child", questionRequested: { interaction: ask } }, "hysec_1")).toBe("descendantAsk")
  expect(askFrameRoute({ session: "hysec_child", interactionResolved: { request: "q1" } }, "hysec_1")).toBe("descendantAsk")
  expect(askFrameRoute({ session: "hysec_child", partAppended: { message: "m", part: "p", textDelta: "x" } }, "hysec_1")).toBe("ignore")
  expect(askFrameRoute({ messageStarted: { message: "m" } }, "hysec_1")).toBe("own")

  const store = open()
  store.applyAsk({ session: "hysec_child", questionRequested: { interaction: ask } })
  expect(store.state.interactions).toEqual([ask])
  // The overlay (transcript) is untouched by a descendant's frame.
  expect(store.state.overlay).toEqual([])
  store.applyAsk({ session: "hysec_child", interactionResolved: { request: "q1" } })
  expect(store.state.interactions).toEqual([])
})

test("a compaction summary message hides the HYA_COMPACTED_CONTEXT marker line", () => {
  const store = open()
  store.setMessages("hysec_1", [{ id: "sum", role: "ROLE_SYSTEM", finish: "FINISH_REASON_STOP", parts: [{ id: "p", text: { text: "HYA_COMPACTED_CONTEXT\nSummary: greeted" } }] }])
  // Its divider (derived from the summary) comes first; the summary shows without the marker.
  const views = transcriptViews(store.state)
  expect(views.map((view) => view.role)).toEqual(["divider", "system"])
  expect(views[1]!.blocks).toEqual([{ kind: "text", id: "p", text: "Summary: greeted" }])
})

test("the status bar shows the WebUI address, or a warning when it is unavailable", () => {
  const base = { mode: "manual", directory: "/w", branch: "main", connected: true }
  expect(statusBarText({ ...base, web: { url: "http://127.0.0.1:3250/" } }, 80)).toBe("mode manual · /w · ⎇ main · WebUI http://127.0.0.1:3250")
  const failed = statusBarSegments({ ...base, web: { error: "port 3250 is in use" } }, 80)
  expect(failed.at(-1)).toEqual({ text: "WebUI unavailable", tone: "warning" })
})
