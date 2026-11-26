// Unit tests for the scriptable OpenAI fake model server (e2e/fake-model.ts):
// Chat Completions and Responses API SSE framing, tool calls, reasoning,
// usage/title isolation, route matching, HTTP error steps, model listing,
// and live hang lifecycle (timeout, abort cancellation, stop cleanup).
// Runs in-process over loopback HTTP without spawning `hya serve` or a PTY.
import { describe, expect, test } from "bun:test"
import {
  hangStep,
  httpErrorStep,
  reasoningStep,
  startFakeModel,
  textStep,
  toolStep,
  type FakeModel,
} from "../e2e/fake-model"

async function postSse(fake: FakeModel, path: string, body: unknown = {}, signal?: AbortSignal): Promise<{ status: number; body: string; frames: unknown[] }> {
  const response = await fetch(`${fake.baseUrl}/${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    ...(signal ? { signal } : {}),
  })
  const text = await response.text()
  const frames: unknown[] = []
  for (const line of text.split("\n")) {
    if (!line.startsWith("data: ")) continue
    const data = line.slice(6).trim()
    if (!data || data === "[DONE]") continue
    frames.push(JSON.parse(data))
  }
  return { status: response.status, body: text, frames }
}

async function pollUntil(fn: () => number, expected: number, timeoutMs = 2_000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (fn() === expected) return
    await Bun.sleep(5)
  }
  expect(fn()).toBe(expected)
}

describe("fake-model protocol framing and routing", () => {
  test("chat/completions streams text chunks, tool calls, usage, and HTTP errors while recording requests", async () => {
    const fake = await startFakeModel([
      textStep("hello-e2e-9f3c", { chunkSize: 5 }),
      toolStep("read", { path: "note.txt" }),
      httpErrorStep(503),
    ])
    fake.setUsage({ prompt: 12, completion: 7, reasoning: 0 })
    try {
      const first = await postSse(fake, "chat/completions", { messages: [{ role: "user", content: "say hi" }] })
      expect(first.status).toBe(200)
      expect(first.body).toContain("data: [DONE]")
      const deltas = first.frames
        .map((frame) => (frame as { choices?: Array<{ delta?: { content?: string } }> }).choices?.[0]?.delta?.content ?? "")
        .join("")
      expect(deltas).toBe("hello-e2e-9f3c")
      const lastFrame = first.frames.at(-1) as { choices?: Array<{ finish_reason?: string }>; usage?: { prompt_tokens?: number; completion_tokens?: number } }
      expect(lastFrame.choices?.[0]?.finish_reason).toBe("stop")
      expect(lastFrame.usage).toMatchObject({ prompt_tokens: 12, completion_tokens: 7 })

      const second = await postSse(fake, "chat/completions", { messages: [{ role: "user", content: "read note" }] })
      const toolDeltas = second.frames.flatMap((frame) => (frame as { choices?: Array<{ delta?: { tool_calls?: Array<{ function?: { name?: string; arguments?: string } }> } }> }).choices?.[0]?.delta?.tool_calls ?? [])
      expect(toolDeltas[0]?.function?.name).toBe("read")
      expect(JSON.parse(toolDeltas[1]?.function?.arguments ?? "{}")).toEqual({ path: "note.txt" })
      expect((second.frames.at(-1) as { choices?: Array<{ finish_reason?: string }> }).choices?.[0]?.finish_reason).toBe("tool_calls")

      const third = await postSse(fake, "chat/completions", { messages: [{ role: "user", content: "fail" }] })
      expect(third.status).toBe(503)
      expect(third.body).toContain("scripted pre-stream failure")

      expect(fake.requests()).toHaveLength(3)
    } finally {
      await fake.stop()
    }
  })

  test("responses streams reasoning before answer text, length incomplete terminal, and function_call items without [DONE]", async () => {
    const fake = await startFakeModel([
      reasoningStep("weighing options quietly", "reason-e2e-4c1d", { chunkSize: 8 }),
      textStep("cut short here", { finish: "length" }),
      toolStep("glob", { pattern: "*.ts" }),
    ])
    try {
      const first = await postSse(fake, "responses", { instructions: "system prompt", input: [{ role: "user", content: "think" }] })
      expect(first.body).not.toContain("[DONE]")
      const types = first.frames.map((f) => (f as { type?: string }).type)
      expect(types[0]).toBe("response.output_item.added")
      expect(types).toContain("response.reasoning_summary_text.delta")
      expect(types).toContain("response.output_text.delta")
      expect(types.at(-1)).toBe("response.completed")
      const reasoning = first.frames
        .filter((f) => (f as { type?: string }).type === "response.reasoning_summary_text.delta")
        .map((f) => (f as { delta?: string }).delta ?? "")
        .join("")
      const answer = first.frames
        .filter((f) => (f as { type?: string }).type === "response.output_text.delta")
        .map((f) => (f as { delta?: string }).delta ?? "")
        .join("")
      expect(reasoning).toBe("weighing options quietly")
      expect(answer).toBe("reason-e2e-4c1d")

      const second = await postSse(fake, "responses", { input: [] })
      expect((second.frames.at(-1) as { type?: string; response?: { status?: string } })).toMatchObject({
        type: "response.incomplete",
        response: { status: "incomplete" },
      })

      const third = await postSse(fake, "responses", { input: [] })
      const doneItem = third.frames.find((f) => (f as { type?: string }).type === "response.output_item.done") as { item?: { type?: string; name?: string; arguments?: string } }
      expect(doneItem.item).toMatchObject({ type: "function_call", name: "glob", arguments: JSON.stringify({ pattern: "*.ts" }) })
      expect(fake.requests()).toHaveLength(3)
    } finally {
      await fake.stop()
    }
  })

  test("routes by system marker, isolates background title requests, and serves GET /v1/models", async () => {
    const fake = await startFakeModel([textStep("shared-reply")])
    fake.route("CHILD_MARKER", [textStep("child-reply")])
    fake.setTitleReply("Generated Title")
    fake.setModelList(["alpha", "beta"])
    try {
      const title = await postSse(fake, "chat/completions", { messages: [{ role: "system", content: "You are a title generator." }] })
      expect(title.body).toContain("Generated Title")
      expect(fake.titleRequests()).toHaveLength(1)
      expect(fake.requests()).toHaveLength(0)

      const child = await postSse(fake, "chat/completions", { messages: [{ role: "system", content: "role: CHILD_MARKER" }] })
      expect(child.body).toContain("child-reply")
      expect(fake.routeRequests("CHILD_MARKER")).toHaveLength(1)

      // Exhausted route does not fall back to the shared queue.
      const exhausted = await postSse(fake, "chat/completions", { messages: [{ role: "system", content: "role: CHILD_MARKER" }] })
      expect(exhausted.body).not.toContain("shared-reply")

      const shared = await postSse(fake, "chat/completions", { messages: [{ role: "user", content: "main" }] })
      expect(shared.body).toContain("shared-reply")

      const models = await fetch(`${fake.baseUrl}/models`, { headers: { authorization: "Bearer test-key" } })
      expect(models.status).toBe(200)
      const list = await models.json() as { data: Array<{ id: string }> }
      expect(list.data.map((m) => m.id)).toEqual(["alpha", "beta"])
      expect(fake.modelListRequests()).toBe(1)
      expect(fake.modelListAuth()).toEqual(["Bearer test-key"])
    } finally {
      await fake.stop()
    }
  })
})

for (const path of ["chat/completions", "responses"] as const) {
  const send = (fake: FakeModel, signal: AbortSignal) => fetch(`${fake.baseUrl}/${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
    signal,
  }).then((response) => response.text())

  describe(`fake model hang lifecycle on ${path}`, () => {
    test("hang timeout removes the request from the pending queue", async () => {
      const fake = await startFakeModel([hangStep(1)])
      try {
        await send(fake, AbortSignal.timeout(5_000))
        expect(fake.pendingHangs()).toBe(0)
      } finally {
        await fake.stop()
      }
    })

    test("cancelling a hang leaves the next request releasable", async () => {
      const fake = await startFakeModel([hangStep(), hangStep()])
      const controller = new AbortController()
      const cancelled = send(fake, controller.signal).catch(() => "cancelled")
      try {
        await pollUntil(() => fake.pendingHangs(), 1)
        controller.abort()
        expect(await cancelled).toBe("cancelled")
        await pollUntil(() => fake.pendingHangs(), 0)

        const next = send(fake, AbortSignal.timeout(5_000))
        await pollUntil(() => fake.pendingHangs(), 1)
        fake.release()
        expect(await next).toContain("data: ")
        expect(fake.pendingHangs()).toBe(0)
      } finally {
        controller.abort()
        fake.release()
        await fake.stop()
      }
    })

    test("stop settles pending hangs and closes their connections", async () => {
      const fake = await startFakeModel([hangStep()])
      const controller = new AbortController()
      const pending = send(fake, controller.signal).catch(() => "disconnected")
      let stopping: Promise<void> | undefined
      try {
        await pollUntil(() => fake.pendingHangs(), 1)
        stopping = fake.stop()
        await pollUntil(() => fake.pendingHangs(), 0)
        await stopping
        await pending
      } finally {
        controller.abort()
        await (stopping ?? fake.stop())
      }
    })
  })
}
