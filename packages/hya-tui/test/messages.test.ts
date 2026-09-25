import { expect, test } from "bun:test"
import type { MessageInfo } from "../src/client"
import { finishNotice, messageView, queuedView, reasoningLabel, shellMarker, toolOutputText, transcriptViews } from "../src/state/messages"
import { createAppStore } from "../src/state/store"

const fallback = { agent: "build", model: "fake/model" }

test("user and assistant messages get distinct roles; assistants carry agent and model", () => {
  const user = messageView({ id: "u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p", text: { text: "hi" } }] }, fallback)
  expect(user.role).toBe("user")
  expect(user.blocks).toEqual([{ kind: "text", id: "p", text: "hi" }])
  expect(user.notice).toBeUndefined()

  const assistant = messageView({ id: "a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "p", text: { text: "**yo**" } }] }, fallback)
  expect(assistant.role).toBe("assistant")
  expect(assistant.agent).toBe("build")
  expect(assistant.model).toBe("fake/model")
  expect(assistant.streaming).toBe(false)
  // A plain stop is not noteworthy: no finish text anywhere.
  expect(assistant.notice).toBeUndefined()
})

test("the message's own agent and model win over the session fallback", () => {
  const view = messageView({ id: "a", role: "ROLE_ASSISTANT", agent: "plan", model: "openai/gpt", parts: [] }, fallback)
  expect(view.agent).toBe("plan")
  expect(view.model).toBe("openai/gpt")
})

test("an assistant message without a finish is streaming", () => {
  const view = messageView({ id: "a", role: "ROLE_ASSISTANT", parts: [{ id: "p", text: { text: "Hel" } }] }, fallback)
  expect(view.streaming).toBe(true)
})

test("finish notices appear only for length, cancelled, and error", () => {
  const base = { id: "a", role: "ROLE_ASSISTANT" }
  expect(finishNotice({ ...base, finish: "FINISH_REASON_STOP" })).toBeUndefined()
  expect(finishNotice({ ...base, finish: "FINISH_REASON_TOOL_CALLS" })).toBeUndefined()
  expect(finishNotice({ ...base, finish: "FINISH_REASON_LENGTH" })).toEqual({ kind: "length", text: "! Reply stopped at the output length limit" })
  expect(finishNotice({ ...base, finish: "FINISH_REASON_CANCELLED" })).toEqual({ kind: "cancelled", text: "! Cancelled" })
  expect(finishNotice({ ...base, finish: "FINISH_REASON_ERROR" })).toEqual({ kind: "error", text: "✗ Turn failed" })
  expect(finishNotice({ ...base, finish: "FINISH_REASON_ERROR", error: { code: "provider_error", message: "http status 400: bad request" } }))
    .toEqual({ kind: "error", text: "✗ provider_error: http status 400: bad request" })
  // An error reported before the finish frame arrives is shown right away.
  expect(finishNotice({ ...base, error: { code: "", message: "boom" } })).toEqual({ kind: "error", text: "✗ boom" })
})

test("reasoning, tool, and attachment parts become typed blocks", () => {
  const view = messageView({
    id: "a", role: "ROLE_ASSISTANT",
    parts: [
      { id: "r", reasoning: { text: "let me think about this" } },
      { id: "t", toolCall: { tool: "bash", state: "TOOL_EXECUTION_STATE_OK" } },
      { id: "x", toolCall: { tool: "read", state: "TOOL_EXECUTION_STATE_ERROR", errorMessage: "missing" } },
      { id: "f", attachment: { name: "notes.md" } },
      { id: "e", text: { text: "" } },
    ],
  }, fallback)
  expect(view.blocks).toEqual([
    { kind: "reasoning", id: "r", text: "let me think about this", words: 5, active: false },
    { kind: "tool", id: "t", tool: "bash", state: "ok" },
    { kind: "tool", id: "x", tool: "read", state: "error", error: "missing" },
    { kind: "attachment", id: "f", name: "notes.md" },
  ])
})

test("reasoning is active while it is the streaming message's last part", () => {
  const streaming = messageView({ id: "a", role: "ROLE_ASSISTANT", parts: [{ id: "r", reasoning: { text: "hmm" } }] }, fallback)
  expect(streaming.blocks[0]).toMatchObject({ kind: "reasoning", active: true })
  const answered = messageView({ id: "a", role: "ROLE_ASSISTANT", parts: [{ id: "r", reasoning: { text: "hmm" } }, { id: "p", text: { text: "ok" } }] }, fallback)
  expect(answered.blocks[0]).toMatchObject({ kind: "reasoning", active: false })
})

test("reasoning labels say whether they are collapsed and how long they are", () => {
  expect(reasoningLabel({ kind: "reasoning", id: "r", text: "a b", words: 2, active: true }, false)).toBe("▸ Thinking… · 2 words")
  expect(reasoningLabel({ kind: "reasoning", id: "r", text: "a", words: 1, active: false }, false)).toBe("▸ Thinking · 1 word")
  expect(reasoningLabel({ kind: "reasoning", id: "r", text: "a", words: 1, active: false }, true)).toBe("▾ Thinking · 1 word")
})

test("views are cached per message object so unchanged messages keep their identity", () => {
  const message: MessageInfo = { id: "a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [] }
  expect(messageView(message, fallback)).toBe(messageView(message, fallback))
  // A different fallback (session model switch) rebuilds the view.
  expect(messageView(message, { agent: "plan", model: "x/y" }).agent).toBe("plan")
})

test("queued prompts are dim user views tagged queued", () => {
  const view = queuedView({ id: 3, session: "s", text: "next", state: "queued" })
  expect(view).toMatchObject({ id: "queued-3", role: "user", queued: true, blocks: [{ kind: "text", id: "queued-3", text: "next" }] })
})

test("the transcript merges the overlay and appends only waiting queued prompts", () => {
  const store = createAppStore()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [] })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w", model: { providerId: "fake", modelId: "model" } })
  store.setMessages("hysec_1", [{ id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p_u", text: { text: "hi" } }] }])
  store.applyEvent({ seq: "5", session: "hysec_1", messageStarted: { message: "m_a", role: "ROLE_ASSISTANT" } })
  store.applyEvent({ session: "hysec_1", partStarted: { message: "m_a", part: "p_a", kind: "text" } })
  store.applyEvent({ session: "hysec_1", partAppended: { message: "m_a", part: "p_a", textDelta: "Hel" } })
  store.flushOverlay()
  const sending = store.enqueue("in flight", "hysec_1")
  store.setQueuedState(sending.id, "sending")
  store.enqueue("later", "hysec_1")
  const views = transcriptViews(store.state)
  expect(views.map((view) => [view.role, view.queued, view.blocks[0]?.kind === "text" ? view.blocks[0].text : ""]))
    .toEqual([["user", false, "hi"], ["assistant", false, "Hel"], ["user", true, "later"]])
  expect(views[1]).toMatchObject({ agent: "build", model: "fake/model", streaming: true })
})

test("a bash tool call shows its command and output when the part carries them", () => {
  const view = messageView({
    id: "a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP",
    parts: [{ id: "t", toolCall: { tool: "bash", state: "TOOL_EXECUTION_STATE_OK", inputJson: "{\"command\":\"echo hello\"}", outputJson: "{\"output\":\"hello\\n\"}" } }],
  }, fallback)
  expect(view.blocks).toEqual([{ kind: "tool", id: "t", tool: "bash", state: "ok", command: "echo hello", output: "hello" }])
})

test("tool output text is taken from a string, a known field, or pretty JSON, and long output is cut", () => {
  expect(toolOutputText("\"plain\"")).toBe("plain")
  expect(toolOutputText("{\"stdout\":\"out\"}")).toBe("out")
  expect(toolOutputText("not json")).toBe("not json")
  expect(toolOutputText("{\"a\":1}")).toBe("{\n  \"a\": 1\n}")
  expect(toolOutputText("")).toBeUndefined()
  const long = Array.from({ length: 30 }, (_, index) => `line ${index + 1}`).join("\n")
  const cut = toolOutputText(JSON.stringify(long))!
  expect(cut.split("\n")).toHaveLength(13)
  expect(cut.endsWith("… 18 more lines")).toBe(true)
})

test("a shell turn run from this TUI shows its command on both messages", () => {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setMessages("hysec_1", [
    { id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p_u", text: { text: shellMarker } }] },
    { id: "m_a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "p_t", toolCall: { tool: "bash", state: "TOOL_EXECUTION_STATE_OK" } }] },
  ])
  store.rememberShell("m_a", "echo hello")
  const views = transcriptViews(store.state)
  expect(views[0]!.blocks).toEqual([{ kind: "text", id: "p_u", text: "!echo hello" }])
  expect(views[1]!.blocks).toEqual([{ kind: "tool", id: "p_t", tool: "bash", state: "ok", command: "echo hello" }])
})

test("a recorded shell turn shows its command from the tool input after a reload", () => {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setMessages("hysec_1", [
    { id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p_u", text: { text: shellMarker } }] },
    { id: "m_a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "p_t", toolCall: { tool: "bash", state: "TOOL_EXECUTION_STATE_OK", inputJson: "{\"command\":\"pwd\"}" } }] },
  ])
  const views = transcriptViews(store.state)
  expect(views[0]!.blocks).toEqual([{ kind: "text", id: "p_u", text: "!pwd" }])
})

test("the running shell turn shows its command before CreateTurn returns", () => {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setPendingShell("sleep 5")
  store.setMessages("hysec_1", [
    { id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p_u", text: { text: shellMarker } }] },
  ])
  expect(transcriptViews(store.state)[0]!.blocks).toEqual([{ kind: "text", id: "p_u", text: "!sleep 5" }])
  store.setMessages("hysec_1", [
    { id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p_u", text: { text: shellMarker } }] },
    { id: "m_a", role: "ROLE_ASSISTANT", parts: [{ id: "p_t", toolCall: { tool: "bash", state: "TOOL_EXECUTION_STATE_RUNNING" } }] },
  ])
  expect(transcriptViews(store.state)[1]!.blocks).toEqual([{ kind: "tool", id: "p_t", tool: "bash", state: "running", command: "sleep 5" }])
})
