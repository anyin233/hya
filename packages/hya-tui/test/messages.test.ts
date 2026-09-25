import { expect, test } from "bun:test"
import type { MessageInfo } from "../src/client"
import { dividerView, finishNotice, lastReplyText, messageView, queuedView, reasoningLabel, shellMarker, toolExpanded, transcriptViews } from "../src/state/messages"
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
  expect(view.blocks).toMatchObject([
    { kind: "reasoning", id: "r", text: "let me think about this", words: 5, active: false },
    { kind: "tool", id: "t", card: { tool: "bash", status: "done" } },
    { kind: "tool", id: "x", card: { tool: "read", status: "failed", error: "missing" } },
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

test("an engine system message gets the system role, not assistant", () => {
  const view = messageView({ id: "s", role: "ROLE_SYSTEM", parts: [{ id: "p", text: { text: "TEAM QUIESCED" } }] }, fallback)
  expect(view.role).toBe("system")
})

test("a divider view carries its text as a single text block under the divider role", () => {
  const view = dividerView({ id: "divider-1", text: "── context compacted · shake ──" })
  expect(view).toMatchObject({ id: "divider-1", role: "divider", blocks: [{ kind: "text", text: "── context compacted · shake ──" }] })
})

test("a compaction divider is spliced right after the message that was newest when it fired", () => {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setMessages("hysec_1", [
    { id: "m1", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p1", text: { text: "hi" } }] },
    { id: "m2", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "p2", text: { text: "hello" } }] },
  ])
  store.applyEvent({ seq: "1", session: "hysec_1", compactionApplied: { untilSeq: "1", strategy: "shake" } })
  const views = transcriptViews(store.state)
  expect(views.map((view) => view.role)).toEqual(["user", "assistant", "divider"])
  expect(views[2]!.blocks[0]).toMatchObject({ text: "── context compacted · shake ──" })
  // A later message after the divider stays after it too.
  store.setMessages("hysec_1", [
    ...store.state.messages,
    { id: "m3", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p3", text: { text: "more" } }] },
  ])
  expect(transcriptViews(store.state).map((view) => view.role)).toEqual(["user", "assistant", "divider", "user"])
})

test("a bash tool call shows its command and output when the part carries them", () => {
  const view = messageView({
    id: "a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP",
    parts: [{ id: "t", toolCall: { tool: "bash", state: "TOOL_EXECUTION_STATE_OK", inputJson: "{\"command\":\"echo hello\"}", outputJson: "{\"output\":\"hello\\n\"}" } }],
  }, fallback)
  expect(view.blocks).toMatchObject([{ kind: "tool", id: "t", card: { tool: "bash", summary: "echo hello", body: [{ text: "$ echo hello" }, { text: "hello" }] } }])
  expect(view.blocks[0]).not.toHaveProperty("shell")
})

test("tool cards are collapsed by default; /tools and a click expand them; shell turns start expanded", () => {
  const store = createAppStore()
  const card = { kind: "tool" as const, id: "t", card: { tool: "read", status: "done" as const, summary: "a", body: [] } }
  expect(toolExpanded(store.state, card)).toBe(false)
  expect(toolExpanded(store.state, { ...card, shell: true })).toBe(true)
  store.toggleTool("t", false)
  expect(toolExpanded(store.state, card)).toBe(true)
  store.setTools(false)
  expect(toolExpanded(store.state, card)).toBe(false)
  store.setTools(true)
  expect(toolExpanded(store.state, card)).toBe(true)
  // The global switch forgets per-card choices, like /thinking.
  store.toggleTool("t", true)
  expect(toolExpanded(store.state, card)).toBe(false)
  store.setTools(false)
  expect(toolExpanded(store.state, { ...card, shell: true })).toBe(false)
})

test("member frames and SessionInfo.members fold into the store", () => {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w", members: [{ member: "mbr_1", child: "hysec_c", agent: "scout", status: "MEMBER_STATUS_SPAWNING" }] })
  expect(store.state.members).toHaveLength(1)
  const effect = store.applyEvent({ seq: "4", session: "hysec_1", memberUpdated: { member: "mbr_1", status: "MEMBER_STATUS_DONE", summary: "ok" } })
  expect(effect.durable).toBe(true)
  expect(store.state.members[0]).toMatchObject({ agent: "scout", status: "MEMBER_STATUS_DONE", summary: "ok" })
  store.setChild("hysec_c", { busy: true, activity: "read a.txt" })
  expect(store.state.children.get("hysec_c")).toEqual({ busy: true, activity: "read a.txt" })
  store.openSession({ id: "hysec_2", agent: "build", workdir: "/w" })
  expect(store.state.members).toEqual([])
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
  expect(views[1]!.blocks).toMatchObject([{ kind: "tool", id: "p_t", shell: true, card: { tool: "bash", status: "done", summary: "echo hello", command: "echo hello" } }])
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
  expect(views[1]!.blocks).toMatchObject([{ kind: "tool", shell: true, card: { summary: "pwd" } }])
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
  expect(transcriptViews(store.state)[1]!.blocks).toMatchObject([{ kind: "tool", id: "p_t", shell: true, card: { status: "running", command: "sleep 5" } }])
})

test("lastReplyText: the text of the newest assistant message with text, blocks joined by a blank line", () => {
  const view = (id: string, role: "ROLE_USER" | "ROLE_ASSISTANT", texts: string[]) =>
    messageView({ id, role, finish: "FINISH_REASON_STOP", parts: texts.map((text, index) => ({ id: `${id}${index}`, text: { text } })) }, fallback)
  expect(lastReplyText([])).toBeUndefined()
  expect(lastReplyText([view("u", "ROLE_USER", ["hi"])])).toBeUndefined()
  expect(lastReplyText([view("a", "ROLE_ASSISTANT", ["one", "two"]), view("u", "ROLE_USER", ["next"])])).toBe("one\n\ntwo")
  // An assistant message with no text (only tool calls) is skipped.
  expect(lastReplyText([view("a", "ROLE_ASSISTANT", ["answer"]), view("b", "ROLE_ASSISTANT", [])])).toBe("answer")
})
