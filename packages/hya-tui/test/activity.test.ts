import { expect, test } from "bun:test"
import { activityText, formatElapsed, workingLineText } from "../src/state/activity"
import { createAppStore } from "../src/state/store"

const session = (id: string, extra: Record<string, unknown> = {}) => ({ id, agent: "build", workdir: "/work", ...extra })

test("formats elapsed time as m:ss, and h:mm:ss past an hour", () => {
  expect(formatElapsed(0)).toBe("0:00")
  expect(formatElapsed(59_000)).toBe("0:59")
  expect(formatElapsed(60_000)).toBe("1:00")
  expect(formatElapsed(125_000)).toBe("2:05")
  expect(formatElapsed(3_661_000)).toBe("1:01:01")
})

test("workingLineText is undefined when no turn admitted by this client runs", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1"))
  expect(workingLineText(store.state, Date.now())).toBeUndefined()
})

test("activityText is undefined with no session open", () => {
  const store = createAppStore()
  expect(activityText(store.state)).toBeUndefined()
})

test("no blocks yet, or between parts, reads Thinking", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1"))
  store.beginTurn()
  store.applyEvent({ seq: "1", session: "hysec_1", messageStarted: { message: "m_a", role: "ROLE_ASSISTANT" } })
  store.flushOverlay()
  expect(activityText(store.state)).toBe("Thinking…")
})

test("a streaming reasoning part reads Thinking; once the answer starts, Writing", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1"))
  store.beginTurn()
  store.applyEvent({ seq: "1", session: "hysec_1", messageStarted: { message: "m_a", role: "ROLE_ASSISTANT" } })
  store.applyEvent({ session: "hysec_1", partStarted: { message: "m_a", part: "p_r", kind: "reasoning" } })
  store.applyEvent({ session: "hysec_1", partAppended: { message: "m_a", part: "p_r", textDelta: "hmm" } })
  store.flushOverlay()
  expect(activityText(store.state)).toBe("Thinking…")

  store.applyEvent({ session: "hysec_1", partStarted: { message: "m_a", part: "p_t", kind: "text" } })
  store.applyEvent({ session: "hysec_1", partAppended: { message: "m_a", part: "p_t", textDelta: "he" } })
  store.flushOverlay()
  expect(activityText(store.state)).toBe("Writing…")
})

test("a running tool call reads Running <tool> <summary>", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1"))
  store.beginTurn()
  store.applyEvent({ seq: "1", session: "hysec_1", messageStarted: { message: "m_a", role: "ROLE_ASSISTANT" } })
  store.applyEvent({ seq: "2", session: "hysec_1", partStarted: { message: "m_a", part: "p_c", kind: "tool_call", tool: "bash", callId: "call_1" } })
  store.applyEvent({ seq: "3", session: "hysec_1", toolStateChanged: { message: "m_a", part: "p_c", state: "TOOL_EXECUTION_STATE_RUNNING", inputJson: "{\"command\":\"echo hi\"}" } })
  store.flushOverlay()
  expect(activityText(store.state)).toBe("Running bash echo hi")
})

test("a pending permission or question prompt outranks any streaming activity", () => {
  const store = createAppStore()
  store.applyCatalog({
    sessions: [session("hysec_1")],
    interactions: [{ id: "perm_1", session: "hysec_1", type: "INTERACTION_TYPE_PERMISSION", title: "bash ls", payload: { action: "run", resource: "bash", tool: "bash" } }],
    models: [], workflows: [], providers: [], savedKeys: [], commands: [],
  })
  store.openSession(session("hysec_1"))
  store.beginTurn()
  expect(activityText(store.state)).toBe("Waiting for approval")

  store.applyCatalog({
    sessions: [session("hysec_1")],
    interactions: [{ id: "que_1", session: "hysec_1", type: "INTERACTION_TYPE_QUESTION", title: "Which one?" }],
    models: [], workflows: [], providers: [], savedKeys: [], commands: [],
  })
  expect(activityText(store.state)).toBe("Waiting for an answer")
})

test("a running task card whose child has not answered reads Waiting for subagent <agent>", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1", { members: [{ member: "mbr_1", child: "hysec_c", agent: "scout", status: "MEMBER_STATUS_RUNNING", callId: "call_1" }] }))
  store.beginTurn()
  store.applyEvent({ seq: "1", session: "hysec_1", messageStarted: { message: "m_a", role: "ROLE_ASSISTANT" } })
  store.applyEvent({ seq: "2", session: "hysec_1", partStarted: { message: "m_a", part: "p_c", kind: "tool_call", tool: "task", callId: "call_1" } })
  store.applyEvent({ seq: "3", session: "hysec_1", toolStateChanged: { message: "m_a", part: "p_c", state: "TOOL_EXECUTION_STATE_RUNNING", inputJson: "{\"subagent_type\":\"scout\",\"description\":\"survey\"}" } })
  store.flushOverlay()
  expect(activityText(store.state)).toBe("Waiting for subagent scout")
})

test("workingLineText adds the elapsed time, Esc hint, and a Queued count", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1"))
  store.beginTurn()
  store.applyEvent({ seq: "1", session: "hysec_1", messageStarted: { message: "m_a", role: "ROLE_ASSISTANT" } })
  store.flushOverlay()
  const started = store.state.turnStartedAt!
  expect(workingLineText(store.state, started + 5000)).toBe("0:05 · Thinking… · Esc to interrupt")
  store.enqueue("next", "hysec_1")
  expect(workingLineText(store.state, started + 5000)).toBe("0:05 · Thinking… · Queued 1 · Esc to interrupt")
})
