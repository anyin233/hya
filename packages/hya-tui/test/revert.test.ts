import { expect, test } from "bun:test"
import { HttpError, type HyaClient, type MessageInfo, type SessionInfo } from "../src/client"
import { createRevertController } from "../src/app/revert"
import { transcriptViews } from "../src/state/messages"
import type { PickerSpec } from "../src/state/picker"
import { forkHeadId, forkRows, forkSourceText, revertIndicator, revertSummary, sessionRow } from "../src/state/revert"
import { createAppStore } from "../src/state/store"

const session = (id: string, extra: Partial<SessionInfo> = {}): SessionInfo => ({ id, agent: "build", workdir: "/work", ...extra })
const user = (id: string, text: string): MessageInfo => ({ id, role: "ROLE_USER", parts: [{ id: `${id}-p`, text: { text } }] })
const reply = (id: string, text: string): MessageInfo => ({ id, role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: `${id}-p`, text: { text } }] })

test("revertSummary counts restored and deleted files and names skipped and failed ones with their reasons", () => {
  expect(revertSummary([], { undone: false, workdir: "/work" })).toBe("Reverted · no file changes")
  expect(revertSummary(undefined, { undone: true, workdir: "/work" })).toBe("Restored · no file changes")
  expect(revertSummary([
    { path: "/work/a.txt", action: "restored" },
    { path: "/work/b.txt", action: "restored" },
    { path: "/work/new.txt", action: "deleted" },
  ], { undone: false, workdir: "/work" })).toBe("Reverted · 2 files restored · 1 deleted")
  expect(revertSummary([
    { path: "/work/a.txt", action: "restored" },
    { path: "/work/big.bin", action: "skipped", reason: "too_large" },
    { path: "/elsewhere/c.txt", action: "failed", reason: "permission denied" },
    { path: "/work/same.txt", action: "unchanged" },
  ], { undone: false, workdir: "/work" }))
    .toBe("Reverted · 1 file restored · 1 unchanged · skipped big.bin (too_large) · failed /elsewhere/c.txt (permission denied)")
  expect(revertSummary([{ path: "/work/a.txt", action: "restored" }], { undone: true, workdir: "/work/" })).toBe("Restored · 1 file restored")
})

test("revertIndicator says how many messages are hidden and how to get them back", () => {
  expect(revertIndicator({ messageId: "m", hiddenMessages: 2 })).toBe("↶ 2 messages reverted · /redo or Ctrl+X R restores them · the next prompt makes it permanent")
  expect(revertIndicator({ messageId: "m", hiddenMessages: 1 })).toBe("↶ 1 message reverted · /redo or Ctrl+X R restores it · the next prompt makes it permanent")
  expect(revertIndicator({ messageId: "m" })).toBe("↶ messages reverted · /redo or Ctrl+X R restores them · the next prompt makes it permanent")
})

test("forkRows: the head first, then the user prompts newest first", () => {
  const store = createAppStore()
  store.openSession(session("s"))
  store.setMessages("s", [user("u1", "first prompt"), reply("a1", "ok"), user("u2", "second\nprompt"), reply("a2", "ok")])
  const rows = forkRows(transcriptViews(store.state))
  expect(rows.map((row) => row.id)).toEqual([forkHeadId, "u2", "u1"])
  expect(rows[0]!.label).toBe("Fork at the latest message")
  expect(rows[1]!.label).toBe("second prompt")
  expect(rows[1]!.tag).toBe("#2")
  expect(rows[2]!.tag).toBe("#1")
  expect(rows[0]!.current).toBe(true)
})

test("forkSourceText names the source session by title, else by id", () => {
  const sessions = [session("hysec_a", { title: "Parser work" })]
  expect(forkSourceText(undefined, sessions)).toBeUndefined()
  expect(forkSourceText({ session: "hysec_a", messageId: "m" }, sessions)).toBe("forked from Parser work")
  expect(forkSourceText({ session: "hysec_gone" }, sessions)).toBe("forked from hysec_gone")
})

test("sessionRow takes revert and forkedFrom from the fresh row, dropping them when it has none", () => {
  const current = session("s", { title: "t", revert: { messageId: "m", text: "hi" }, forkedFrom: { session: "o" } })
  expect(sessionRow(current, session("s", { title: "t2" }))).toEqual(session("s", { title: "t2" }))
  const next = sessionRow(current, session("s", { revert: { messageId: "m2" } }))
  expect(next.revert).toEqual({ messageId: "m2" })
  expect(next.title).toBe("t")
})

test("store: a durable sessionReverted drops the stale overlay; the next messageStarted commits (clears) the revert", () => {
  const store = createAppStore()
  store.openSession(session("s", { lastSeq: "5" }))
  store.applyEvent({ seq: "6", session: "s", messageStarted: { message: "u1", role: "ROLE_USER" } })
  store.flushOverlay()
  expect(store.state.overlay.length).toBe(1)
  store.applyRevert(session("s", { revert: { messageId: "u1", text: "hi", hiddenMessages: 1 } }))
  expect(store.state.overlay).toEqual([])
  expect(store.state.selected?.revert?.messageId).toBe("u1")
  store.applyEvent({ seq: "7", session: "s", messageStarted: { message: "u9", role: "ROLE_USER" } })
  store.flushOverlay()
  store.applyEvent({ seq: "8", session: "s", sessionReverted: { messageId: "u9", files: [] } })
  expect(store.state.overlay).toEqual([])
  expect(store.fold.lastSeq).toBe("8")
  store.applyRevert(session("s", { revert: { messageId: "u9" } }))
  store.applyEvent({ seq: "9", session: "s", messageStarted: { message: "u10", role: "ROLE_USER" } })
  expect(store.state.selected?.revert).toBeUndefined()
})

test("store: setSessions drops a revert the fresh row no longer has", () => {
  const store = createAppStore()
  store.openSession(session("s", { revert: { messageId: "m" } }))
  store.setSessions([session("s")])
  expect(store.state.selected?.revert).toBeUndefined()
})

test("the transcript ends with the pending-revert indicator", () => {
  const store = createAppStore()
  store.openSession(session("s", { revert: { messageId: "u2", hiddenMessages: 2 } }))
  store.setMessages("s", [user("u1", "first"), reply("a1", "ok")])
  const views = transcriptViews(store.state)
  const last = views.at(-1)!
  expect(last.id).toBe("revert-pending")
  expect(last.role).toBe("divider")
  expect(last.tone).toBe("warning")
  expect(last.blocks[0]).toMatchObject({ kind: "text", text: revertIndicator({ messageId: "u2", hiddenMessages: 2 }) })
})

function harness(overrides: Partial<HyaClient> = {}) {
  const store = createAppStore()
  let input = ""
  const calls: string[] = []
  const pickers: PickerSpec[] = []
  const client = {
    revertSession: async (id: string, body: { messageId?: string; undo?: boolean }) => {
      calls.push(`revert ${id} ${JSON.stringify(body)}`)
      if (body.undo) return { session: session(id), files: [{ path: "/work/a.txt", action: "restored" }] }
      return { session: session(id, { revert: { messageId: "u2", text: "second prompt", hiddenMessages: 2 } }), files: [{ path: "/work/a.txt", action: "deleted" }] }
    },
    forkSession: async (id: string, messageId?: string) => {
      calls.push(`fork ${id} ${messageId ?? "head"}`)
      return { session: session("hysec_fork", { title: "Fork" }), promptText: messageId ? "second prompt" : "" }
    },
    listMessages: async (id: string) => {
      calls.push(`messages ${id}`)
      return [user("u1", "first prompt"), reply("a1", "ok")]
    },
    ...overrides,
  } as unknown as HyaClient
  const revert = createRevertController({
    store,
    client,
    composer: () => ({ text: () => input, setText: (text: string) => { input = text } }),
    openSession: async (id) => { calls.push(`open ${id}`); store.openSession(session(id)) },
    refresh: async () => { calls.push("refresh") },
    openPicker: (spec) => { pickers.push(spec) },
  })
  store.openSession(session("s"))
  store.setMessages("s", [user("u1", "first prompt"), reply("a1", "ok"), user("u2", "second prompt"), reply("a2", "done")])
  return { store, calls, pickers, revert, input: () => input, setInput: (text: string) => { input = text } }
}

test("/undo reverts the last prompt, reloads the transcript, and puts the prompt back in an empty input", async () => {
  const { store, calls, revert, input } = harness()
  await revert.undo()
  expect(calls).toEqual(['revert s {}', "messages s"])
  expect(store.state.messages.map((message) => message.id)).toEqual(["u1", "a1"])
  expect(store.state.selected?.revert?.messageId).toBe("u2")
  expect(input()).toBe("second prompt")
  expect(store.state.status).toBe("Reverted · 1 deleted · the prompt is back in the input")
})

test("/undo keeps text the user typed; a second /undo replaces the untouched prefill", async () => {
  const typed = harness()
  typed.setInput("my draft")
  await typed.revert.undo()
  expect(typed.input()).toBe("my draft")
  expect(typed.store.state.status).toBe("Reverted · 1 deleted · the input kept your text")

  let text = "second prompt"
  const again = harness({
    revertSession: async () => ({ session: session("s", { revert: { messageId: "u1", text, hiddenMessages: 4 } }), files: [] }),
  } as Partial<HyaClient>)
  await again.revert.undo()
  expect(again.input()).toBe("second prompt")
  text = "first prompt"
  await again.revert.undo()
  expect(again.input()).toBe("first prompt")
})

test("/undo while a turn runs shows the server's refusal; nothing to undo says so", async () => {
  const busy = harness({ revertSession: async () => { throw new HttpError(409, "POST", "/v1/x", "session_busy: a turn is running") } } as Partial<HyaClient>)
  await busy.revert.undo()
  expect(busy.store.state.status).toBe("Undo refused: a turn is running · wait for it to finish or press Esc to cancel it")
  const none = harness({ revertSession: async () => { throw new HttpError(400, "POST", "/v1/x", "invalid_argument: no user message to revert") } } as Partial<HyaClient>)
  await none.revert.undo()
  expect(none.store.state.status).toBe("Nothing to undo: invalid_argument: no user message to revert")
})

test("/undo in a subagent's view or without a session does not call the server", async () => {
  const child = harness()
  child.store.openSession(session("c", { parent: "s" }))
  await child.revert.undo()
  expect(child.calls).toEqual([])
  expect(child.store.state.status).toContain("Read-only")
  const empty = harness()
  empty.store.clearSelected()
  await empty.revert.undo()
  expect(empty.calls).toEqual([])
  expect(empty.store.state.status).toBe("Nothing to undo: no session is open")
})

test("/redo only while a revert is pending; it restores and clears the untouched prefill", async () => {
  const { store, calls, revert, input, setInput } = harness()
  await revert.redo()
  expect(calls).toEqual([])
  expect(store.state.status).toBe("Nothing to redo · /redo works after /undo, until the next prompt")

  await revert.undo()
  expect(input()).toBe("second prompt")
  calls.length = 0
  await revert.redo()
  expect(calls).toEqual(['revert s {"undo":true}', "messages s"])
  expect(store.state.selected?.revert).toBeUndefined()
  expect(input()).toBe("")
  expect(store.state.status).toBe("Restored · 1 file restored")

  await revert.undo()
  setInput("second prompt, edited")
  await revert.redo()
  expect(input()).toBe("second prompt, edited")
})

test("/redo refused by the server (the revert was committed) drops the local revert", async () => {
  const { store, revert } = harness({ revertSession: async () => { throw new HttpError(400, "POST", "/v1/x", "invalid_argument: nothing to undo") } } as Partial<HyaClient>)
  store.setSelected(session("s", { revert: { messageId: "u2" } }))
  await revert.redo()
  expect(store.state.selected?.revert).toBeUndefined()
  expect(store.state.status).toBe("Nothing to redo: invalid_argument: nothing to undo")
})

test("/fork opens a picker of the prompts; choosing one forks before it, switches, and prefills", async () => {
  const { store, calls, pickers, revert, input } = harness()
  revert.fork()
  expect(pickers.length).toBe(1)
  const spec = pickers[0]!
  expect(spec.rows.map((row) => row.id)).toEqual([forkHeadId, "u2", "u1"])
  await spec.onSelect(spec.rows[1]!)
  expect(calls).toEqual(["fork s u2", "refresh", "open hysec_fork"])
  expect(store.state.selected?.id).toBe("hysec_fork")
  expect(input()).toBe("second prompt")
  expect(store.state.status).toBe("Forked before “second prompt” · the prompt is in the input")
})

test("/fork at the head copies everything and leaves the input alone", async () => {
  const { store, calls, pickers, revert, input } = harness()
  revert.fork()
  await pickers[0]!.onSelect(pickers[0]!.rows[0]!)
  expect(calls).toEqual(["fork s head", "refresh", "open hysec_fork"])
  expect(input()).toBe("")
  expect(store.state.status).toBe("Forked at the latest message")
})

test("/fork without a session says so", () => {
  const { store, pickers, revert } = harness()
  store.clearSelected()
  revert.fork()
  expect(pickers).toEqual([])
  expect(store.state.status).toBe("Nothing to fork: no session is open")
})
