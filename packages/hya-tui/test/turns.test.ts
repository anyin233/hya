import { expect, test } from "bun:test"
import { createTurnRunner } from "../src/app/turns"
import { HttpError, type StreamEvent, type TurnInfo } from "../src/client"
import { createAppStore } from "../src/state/store"

const S = "hysec_1"

type Reply = TurnInfo | HttpError | Error

function harness(replies: Reply[] = []) {
  const store = createAppStore()
  store.openSession({ id: S, agent: "build", workdir: "/w" })
  const sent: string[] = []
  const delays: number[] = []
  let seq = 0
  let userCount = 0
  const queue = [...replies]
  const runner = createTurnRunner({
    store,
    client: {
      createTurn: async (_session: string, text: string) => {
        sent.push(text)
        const next = queue.shift() ?? { id: `msg_u${++userCount}`, state: "TURN_STATE_RUNNING" }
        if (next instanceof Error) throw next
        return next
      },
    },
    sleep: async (ms) => { delays.push(ms) },
    backoffMs: [50, 100, 200],
  })
  const emit = (payload: Omit<StreamEvent, "seq" | "session">) => {
    const effect = store.applyEvent({ seq: String(++seq), session: S, ...payload })
    runner.observe(effect)
  }
  /** The durable frames of one admitted turn: user message, then assistant rounds with finishes. */
  const turn = (user: string, finishes: string[], error?: { code: string; errorMessage: string }) => {
    emit({ messageStarted: { message: user, role: "ROLE_USER" } })
    emit({ messageFinished: { message: user, finish: "FINISH_REASON_STOP" } })
    finishes.forEach((finish, index) => {
      const id = `${user}_a${index}`
      emit({ messageStarted: { message: id, role: "ROLE_ASSISTANT" } })
      if (error && index === finishes.length - 1) emit({ errorReported: { message: id, ...error } })
      emit({ messageFinished: { message: id, finish } })
    })
  }
  return { store, runner, sent, delays, emit, turn }
}

const busy = () => new HttpError(409, "POST", `/v1/sessions/${S}/turns`, "session_busy: session busy")

test("an idle prompt is sent at once and marks the turn running", async () => {
  const { store, runner, sent } = harness([{ id: "msg_u1", state: "TURN_STATE_RUNNING" }])
  await runner.submit("first")
  expect(sent).toEqual(["first"])
  expect(store.state.running).toBe(true)
  expect(store.state.turnId).toBe("msg_u1")
  expect(store.state.queued).toEqual([])
  expect(store.state.status).toBe("Running · msg_u1")
})

test("the turn ends on the final assistant messageFinished, not on the returned (user message) id", async () => {
  const { store, runner, emit } = harness([{ id: "msg_u1", state: "TURN_STATE_RUNNING" }])
  await runner.submit("first")
  emit({ messageStarted: { message: "msg_u1", role: "ROLE_USER" } })
  emit({ messageFinished: { message: "msg_u1", finish: "FINISH_REASON_STOP" } })
  expect(store.state.running).toBe(true)
  emit({ messageStarted: { message: "msg_a1", role: "ROLE_ASSISTANT" } })
  emit({ messageFinished: { message: "msg_a1", finish: "FINISH_REASON_TOOL_CALLS" } })
  expect(store.state.running).toBe(true)
  emit({ messageStarted: { message: "msg_a2", role: "ROLE_ASSISTANT" } })
  emit({ messageFinished: { message: "msg_a2", finish: "FINISH_REASON_STOP" } })
  await runner.idle()
  expect(store.state.running).toBe(false)
  expect(store.state.turnId).toBe("")
  expect(store.state.status).toBe("Ready")
})

test("a reply that finishes before CreateTurn returns still ends the turn", async () => {
  const { store, runner, turn } = harness()
  const pending = runner.submit("fast")
  turn("msg_u1", ["FINISH_REASON_STOP"])
  await pending
  await runner.idle()
  expect(store.state.running).toBe(false)
  expect(store.state.status).toBe("Ready")
})

test("prompts submitted during a running turn queue in order and are sent after each turn end", async () => {
  const { store, runner, sent, turn } = harness()
  await runner.submit("one")
  await runner.submit("two")
  await runner.submit("three")
  expect(sent).toEqual(["one"])
  expect(store.state.queued.map((item) => [item.text, item.state])).toEqual([["two", "queued"], ["three", "queued"]])
  expect(store.state.status).toBe("Running · msg_u1 · 2 queued")
  turn("msg_u1", ["FINISH_REASON_STOP"])
  await runner.idle()
  expect(sent).toEqual(["one", "two"])
  expect(store.state.queued.map((item) => item.text)).toEqual(["three"])
  expect(store.state.turnId).toBe("msg_u2")
  turn("msg_u2", ["FINISH_REASON_STOP"])
  await runner.idle()
  expect(sent).toEqual(["one", "two", "three"])
  expect(store.state.queued).toEqual([])
  turn("msg_u3", ["FINISH_REASON_STOP"])
  await runner.idle()
  expect(store.state.status).toBe("Ready")
})

test("409 session_busy is retried with backoff until the run guard releases", async () => {
  const { store, runner, sent, delays } = harness([busy(), busy(), { id: "msg_u1", state: "TURN_STATE_RUNNING" }])
  await runner.submit("hello")
  expect(sent).toEqual(["hello", "hello", "hello"])
  expect(delays).toEqual([50, 100])
  expect(store.state.turnId).toBe("msg_u1")
  expect(store.state.queued).toEqual([])
})

test("a prompt stays queued when the session stays busy, and is sent after the next turn end", async () => {
  const { store, runner, sent, delays, turn } = harness([busy(), busy(), busy(), busy()])
  await runner.submit("waiting")
  expect(delays).toEqual([50, 100, 200])
  expect(store.state.running).toBe(false)
  expect(store.state.queued.map((item) => [item.text, item.state])).toEqual([["waiting", "queued"]])
  expect(store.state.status).toBe("Session busy · 1 queued prompt waits for the running turn")
  turn("msg_foreign", ["FINISH_REASON_STOP"])
  await runner.idle()
  expect(sent).toEqual(["waiting", "waiting", "waiting", "waiting", "waiting"])
  expect(store.state.queued).toEqual([])
  expect(store.state.running).toBe(true)
})

test("other admission errors drop the prompt and report the error", async () => {
  const { store, runner } = harness([new Error("boom")])
  await runner.submit("bad")
  expect(store.state.running).toBe(false)
  expect(store.state.queued).toEqual([])
  expect(store.state.status).toBe("Error: Error: boom")
})

test("a cancelled turn shows a non-running state", async () => {
  const { store, runner, turn } = harness()
  await runner.submit("stop me")
  turn("msg_u1", ["FINISH_REASON_CANCELLED"])
  await runner.idle()
  expect(store.state.running).toBe(false)
  expect(store.state.status).toBe("Cancelled · Ready")
})

test("a failed turn shows the recorded error text", async () => {
  const { store, runner, turn } = harness()
  await runner.submit("fail")
  turn("msg_u1", ["FINISH_REASON_ERROR"], { code: "provider_error", errorMessage: "http status 400: scripted" })
  await runner.idle()
  expect(store.state.running).toBe(false)
  expect(store.state.status).toBe("Error · provider_error: http status 400: scripted")
})

test("a failed turn without a recorded error falls back to a generic error state", async () => {
  const { store, runner, turn } = harness()
  await runner.submit("fail")
  turn("msg_u1", ["FINISH_REASON_ERROR"])
  await runner.idle()
  expect(store.state.status).toBe("Error · turn failed")
})

test("opening another session clears the queue and the turn state", async () => {
  const { store, runner } = harness()
  await runner.submit("one")
  await runner.submit("two")
  store.openSession({ id: "hysec_2", agent: "build", workdir: "/w" })
  expect(store.state.queued).toEqual([])
  expect(store.state.running).toBe(false)
  expect(store.state.turnId).toBe("")
  expect(store.state.overlay).toEqual([])
})
