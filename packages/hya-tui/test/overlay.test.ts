import { expect, test } from "bun:test"
import type { MessageInfo, StreamEvent } from "../src/client"
import { mergeTranscript, TranscriptOverlay } from "../src/state/overlay"

const S = "hysec_1"
const ev = (seq: number, payload: Omit<StreamEvent, "seq" | "session">): StreamEvent =>
  ({ ...(seq ? { seq: String(seq) } : {}), session: S, ...payload })
const live = (payload: Omit<StreamEvent, "seq" | "session">): StreamEvent => ev(0, payload)

const started = (message: string, role: string) => ({ messageStarted: { message, role } })
const partStarted = (message: string, part: string, kind = "text") => ({ partStarted: { message, part, kind } })
const appended = (message: string, part: string, textDelta: string) => ({ partAppended: { message, part, textDelta } })
const replaced = (message: string, part: string, text: string) => ({ partReplaced: { message, part, text } })
const completed = (message: string, part: string) => ({ partCompleted: { message, part } })
const finished = (message: string, finish: string, cause?: string) => ({ messageFinished: { message, finish, ...(cause ? { cause } : {}) } })

function textOf(messages: MessageInfo[], id: string): string[] {
  return (messages.find((message) => message.id === id)?.parts ?? []).map((part) => part.text?.text ?? "")
}

/** Live deltas for one assistant text part, then its durable record, as the v1 stream sends them. */
function streamReply(overlay: TranscriptOverlay, firstSeq: number, text: string[]): void {
  overlay.apply(ev(firstSeq, started("m_a", "ROLE_ASSISTANT")))
  overlay.apply(live(partStarted("m_a", "p_a")))
  for (const chunk of text) overlay.apply(live(appended("m_a", "p_a", chunk)))
  overlay.apply(live(completed("m_a", "p_a")))
}

test("live deltas grow one text part chunk by chunk", () => {
  const overlay = new TranscriptOverlay("0")
  overlay.apply(ev(7, started("m_a", "ROLE_ASSISTANT")))
  overlay.apply(live(partStarted("m_a", "p_a")))
  overlay.apply(live(appended("m_a", "p_a", "Hel")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["Hel"])
  overlay.apply(live(appended("m_a", "p_a", "lo")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["Hello"])
  expect(overlay.messages()[0]?.role).toBe("ROLE_ASSISTANT")
})

test("the durable partStarted/partReplaced of a live part does not double its text", () => {
  const overlay = new TranscriptOverlay("0")
  streamReply(overlay, 7, ["Hello ", "world"])
  overlay.apply(ev(12, partStarted("m_a", "p_a")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["Hello world"])
  overlay.apply(ev(13, replaced("m_a", "p_a", "Hello world")))
  overlay.apply(ev(14, completed("m_a", "p_a")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["Hello world"])
  expect(overlay.lastSeq).toBe("14")
})

test("partReplaced sets the whole text, superseding live deltas (plugin rewrite)", () => {
  const overlay = new TranscriptOverlay("0")
  streamReply(overlay, 3, ["draft ", "text"])
  overlay.apply(live(replaced("m_a", "p_a", "rewritten")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["rewritten"])
  overlay.apply(ev(9, replaced("m_a", "p_a", "rewritten")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["rewritten"])
})

test("a repeated partStarted for a known id is not a new part", () => {
  const overlay = new TranscriptOverlay("0")
  overlay.apply(ev(2, started("m_a", "ROLE_ASSISTANT")))
  overlay.apply(live(partStarted("m_a", "p_a")))
  overlay.apply(live(partStarted("m_a", "p_a")))
  overlay.apply(ev(3, partStarted("m_a", "p_a")))
  expect(overlay.messages()[0]?.parts?.length).toBe(1)
})

test("durable frames at or below the last applied seq are ignored; live frames have no seq", () => {
  const overlay = new TranscriptOverlay("10")
  const stale = overlay.apply(ev(10, started("m_old", "ROLE_USER")))
  expect(stale.changed).toBe(false)
  expect(overlay.messages()).toEqual([])
  overlay.apply(ev(11, started("m_u", "ROLE_USER")))
  overlay.apply(ev(12, partStarted("m_u", "p_u")))
  overlay.apply(ev(13, appended("m_u", "p_u", "hi")))
  overlay.apply(ev(13, appended("m_u", "p_u", "hi")))
  expect(textOf(overlay.messages(), "m_u")).toEqual(["hi"])
  expect(overlay.lastSeq).toBe("13")
  overlay.apply(live(started("m_x", "ROLE_ASSISTANT")))
  expect(overlay.lastSeq).toBe("13")
})

test("keeps 64-bit sequence numbers exact", () => {
  const overlay = new TranscriptOverlay("9007199254740993")
  expect(overlay.apply(ev(0, {})).changed).toBe(false)
  overlay.apply({ seq: "9007199254740993", session: S, ...started("m_dup", "ROLE_USER") })
  expect(overlay.messages()).toEqual([])
  overlay.apply({ seq: "9007199254740994", session: S, ...started("m_new", "ROLE_USER") })
  expect(overlay.lastSeq).toBe("9007199254740994")
})

test("after a resync, live deltas of an interrupted part stop until its durable text arrives", () => {
  const overlay = new TranscriptOverlay("0")
  streamReply(overlay, 5, ["Hel"])
  overlay.markLiveLost()
  overlay.apply(live(appended("m_a", "p_a", "XYZ")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["Hel"])
  overlay.apply(ev(8, replaced("m_a", "p_a", "Hello there")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["Hello there"])
  // A part that starts after the resync streams normally again.
  overlay.apply(live(partStarted("m_a", "p_b")))
  overlay.apply(live(appended("m_a", "p_b", "next")))
  expect(textOf(overlay.messages(), "m_a")).toEqual(["Hello there", "next"])
})

test("reset clears the overlay for a session switch and restarts from the new seq", () => {
  const overlay = new TranscriptOverlay("0")
  streamReply(overlay, 5, ["abc"])
  overlay.reset("40")
  expect(overlay.messages()).toEqual([])
  expect(overlay.lastSeq).toBe("40")
  expect(overlay.apply(ev(39, started("m_z", "ROLE_USER"))).changed).toBe(false)
})

test("records errorReported on the message and reports the final finish", () => {
  const overlay = new TranscriptOverlay("0")
  overlay.apply(ev(2, started("m_u", "ROLE_USER")))
  overlay.apply(ev(3, finished("m_u", "FINISH_REASON_STOP")))
  overlay.apply(ev(4, started("m_a", "ROLE_ASSISTANT")))
  overlay.apply(ev(5, { errorReported: { message: "m_a", code: "provider_error", errorMessage: "http status 400: bad" } }))
  const effect = overlay.apply(ev(6, finished("m_a", "FINISH_REASON_ERROR", "FINISH_CAUSE_PROVIDER_ERROR")))
  expect(effect.finished).toEqual({ message: "m_a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_ERROR", cause: "FINISH_CAUSE_PROVIDER_ERROR" })
  expect(overlay.messages().find((message) => message.id === "m_a")?.error).toEqual({ code: "provider_error", message: "http status 400: bad" })
})

test("turnEnd finds the final assistant finish after a user message, skipping tool-call rounds", () => {
  const overlay = new TranscriptOverlay("0")
  overlay.apply(ev(1, started("m_u", "ROLE_USER")))
  overlay.apply(ev(2, finished("m_u", "FINISH_REASON_STOP")))
  expect(overlay.turnEnd("m_u")).toBeUndefined()
  overlay.apply(ev(3, started("m_a1", "ROLE_ASSISTANT")))
  overlay.apply(ev(4, finished("m_a1", "FINISH_REASON_TOOL_CALLS")))
  expect(overlay.turnEnd("m_u")).toBeUndefined()
  overlay.apply(ev(5, started("m_a2", "ROLE_ASSISTANT")))
  overlay.apply(ev(6, finished("m_a2", "FINISH_REASON_STOP")))
  expect(overlay.turnEnd("m_u")?.message).toBe("m_a2")
  expect(overlay.knows("m_u")).toBe(true)
  expect(overlay.knows("m_other")).toBe(false)
})

test("merge: the streaming overlay extends the projection without duplicating messages or parts", () => {
  const overlay = new TranscriptOverlay("0")
  overlay.apply(ev(1, started("m_u", "ROLE_USER")))
  overlay.apply(ev(2, partStarted("m_u", "p_u")))
  overlay.apply(ev(3, appended("m_u", "p_u", "hi")))
  overlay.apply(ev(4, finished("m_u", "FINISH_REASON_STOP")))
  streamReply(overlay, 5, ["Hello", " wor"])
  const projection: MessageInfo[] = [
    { id: "m_u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p_u", text: { text: "hi" } }] },
    { id: "m_a", role: "ROLE_ASSISTANT", parts: [{ id: "p_tool", toolCall: { tool: "read" } }] },
  ]
  const merged = mergeTranscript(projection, overlay.messages())
  expect(merged.map((message) => message.id)).toEqual(["m_u", "m_a"])
  expect(merged[0]).toBe(projection[0]!)
  expect(merged[1]?.parts?.map((part) => part.id)).toEqual(["p_tool", "p_a"])
  expect(textOf(merged, "m_a")).toEqual(["", "Hello wor"])
})

test("merge: overlay-only messages follow the projection; a finished projected message wins", () => {
  const overlay = new TranscriptOverlay("0")
  streamReply(overlay, 5, ["partial"])
  const before = mergeTranscript([], overlay.messages())
  expect(textOf(before, "m_a")).toEqual(["partial"])
  const projection: MessageInfo[] = [{
    id: "m_a", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "p_a", text: { text: "partial and final" } }],
  }]
  expect(mergeTranscript(projection, overlay.messages())).toEqual(projection)
  overlay.prune(projection)
  expect(overlay.messages()).toEqual([])
})

test("snapshots reuse unchanged message objects so formatting can be cached", () => {
  const overlay = new TranscriptOverlay("0")
  overlay.apply(ev(1, started("m_u", "ROLE_USER")))
  streamReply(overlay, 2, ["a"])
  const first = overlay.messages()
  overlay.apply(live(appended("m_a", "p_a", "b")))
  const second = overlay.messages()
  expect(second[0]).toBe(first[0]!)
  expect(second[1]).not.toBe(first[1]!)
})
