// Verifies the fake OpenAI model (fake-model.ts) and the `model` backend
// fixture option through the v1 HTTP API directly, with no TUI rendering
// involved: a scripted text reply becomes the assistant message, a scripted
// tool call round-trips through a real builtin tool and a follow-up text step
// finishes the turn, the Responses protocol turns a reasoning step into a
// reasoning part and a `length` finish into FINISH_REASON_LENGTH, and the
// fake server records what it received.

import { expect, reasoningStep, textStep, test, toolStep, type Backend } from "./hya"

type SessionInfo = { id: string }
type TurnInfo = { id: string; state: string }
type MessagePart = {
  text?: { text: string }
  reasoning?: { text: string }
  toolCall?: { tool: string; inputJson?: string; state?: string }
  toolResult?: { output: string }
}
type MessageInfo = { id: string; role: string; finish?: string; parts?: MessagePart[] }

async function api<T>(backend: Backend, method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${backend.url}${path}`, {
    method,
    headers: {
      "x-hya-directory": backend.dir,
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  })
  const text = await response.text()
  if (!response.ok) throw new Error(`${method} ${path}: HTTP ${response.status} ${text}`)
  return text ? (JSON.parse(text) as T) : (undefined as T)
}

async function createSession(backend: Backend): Promise<SessionInfo> {
  const result = await api<{ session: SessionInfo }>(backend, "POST", "/v1/sessions", {
    agent: "build",
    model: "fake/model",
    workdir: backend.dir,
  })
  return result.session
}

async function createTurn(backend: Backend, session: string, text: string): Promise<TurnInfo> {
  const result = await api<{ turn: TurnInfo }>(backend, "POST", `/v1/sessions/${session}/turns`, {
    prompt: { text },
  })
  return result.turn
}

async function waitTurn(backend: Backend, session: string, turn: string): Promise<TurnInfo> {
  return api<TurnInfo>(backend, "POST", `/v1/sessions/${session}/turns/${turn}/wait`, { timeoutMs: 20_000 })
}

async function listMessages(backend: Backend, session: string): Promise<MessageInfo[]> {
  const result = await api<{ messages: MessageInfo[] }>(backend, "GET", `/v1/sessions/${session}/messages`)
  return result.messages
}

test.describe("fake OpenAI-compatible model", () => {
  test.use({ model: { steps: [textStep("the fake model says hello-e2e-9f3c")] } })

  test("a scripted text reply arrives as the assistant message", async ({ backend, fakeModel }) => {
    const session = await createSession(backend)
    const turn = await createTurn(backend, session.id, "say hi")
    const finished = await waitTurn(backend, session.id, turn.id)
    expect(finished.state).toBe("TURN_STATE_FINISHED")

    const messages = await listMessages(backend, session.id)
    const assistant = messages.find((m) => m.role === "ROLE_ASSISTANT")
    const text = assistant?.parts?.map((p) => p.text?.text ?? "").join("") ?? ""
    expect(text).toContain("hello-e2e-9f3c")

    expect(fakeModel!.requests().length).toBe(1)
  })
})

test.describe("fake model tool calls", () => {
  test.use({
    model: {
      steps: [toolStep("read", { path: "note.txt" }), textStep("read note.txt and it said hello-tool-af21")],
    },
  })

  test("a scripted tool call round-trips through a real tool, then a follow-up step finishes the turn", async ({
    backend,
    fakeModel,
  }) => {
    const { writeFile } = await import("node:fs/promises")
    const { join } = await import("node:path")
    await writeFile(join(backend.dir, "note.txt"), "hello-tool-af21\n")

    const session = await createSession(backend)
    const turn = await createTurn(backend, session.id, "read note.txt")
    const finished = await waitTurn(backend, session.id, turn.id)
    expect(finished.state).toBe("TURN_STATE_FINISHED")

    const messages = await listMessages(backend, session.id)
    const assistant = messages.find((m) => m.role === "ROLE_ASSISTANT")
    const parts = assistant?.parts ?? []
    // ListMessages folds a completed tool call into one `toolCall` part
    // carrying its execution state; the raw output is not echoed back as a
    // separate `toolResult` part on this read (the follow-up text step below
    // is itself proof the engine fed a result back to the model).
    const toolCall = parts.find((p) => p.toolCall?.tool === "read")
    expect(toolCall).toBeTruthy()
    expect(toolCall?.toolCall?.state).toBe("TOOL_EXECUTION_STATE_OK")
    const text = parts.map((p) => p.text?.text ?? "").join("")
    expect(text).toContain("hello-tool-af21")

    // Two model requests: the tool-call turn, then the follow-up text turn
    // after the engine fed the tool result back in.
    expect(fakeModel!.requests().length).toBe(2)
  })
})

test.describe("fake model on the Responses protocol", () => {
  test.use({
    model: {
      protocol: "responses",
      steps: [
        reasoningStep("weighing the options quietly", "the answer is reason-e2e-4c1d", { chunkSize: 8 }),
        textStep("cut short here", { finish: "length" }),
      ],
    },
  })

  test("a reasoning step becomes a reasoning part before the answer, and a length finish is recorded", async ({ backend, fakeModel }) => {
    const session = await createSession(backend)
    const first = await createTurn(backend, session.id, "think first")
    expect((await waitTurn(backend, session.id, first.id)).state).toBe("TURN_STATE_FINISHED")
    let messages = await listMessages(backend, session.id)
    const reply = messages.filter((m) => m.role === "ROLE_ASSISTANT").at(-1)!
    const kinds = reply.parts?.map((p) => (p.reasoning ? "reasoning" : p.text ? "text" : "other"))
    expect(kinds).toEqual(["reasoning", "text"])
    expect(reply.parts?.[0]?.reasoning?.text).toBe("weighing the options quietly")
    expect(reply.parts?.[1]?.text?.text).toBe("the answer is reason-e2e-4c1d")

    const second = await createTurn(backend, session.id, "again")
    await waitTurn(backend, session.id, second.id)
    messages = await listMessages(backend, session.id)
    expect(messages.filter((m) => m.role === "ROLE_ASSISTANT").at(-1)?.finish).toBe("FINISH_REASON_LENGTH")
    // hya sent both requests to the Responses route, with its system prompt as `instructions`.
    const bodies = fakeModel!.requests() as Array<{ input?: unknown; instructions?: unknown }>
    expect(bodies.length).toBe(2)
    expect(Array.isArray(bodies[0]!.input)).toBe(true)
  })
})
