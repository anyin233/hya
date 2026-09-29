// An empty thinking block (a model that opens a reasoning item and writes no
// summary) is stored with empty text, which protojson omits (`reasoning: {}`).
// Opening such a session must still render its transcript: before the fix the
// TUI's refresh threw "TypeError … text.split" and showed "Refresh failed".
import { api, expect, hyaTui, reasoningStep, test } from "./hya"

test.describe("empty reasoning", () => {
  test.use({ model: { protocol: "responses", steps: [reasoningStep("", "Answer after silent thinking.")] } })

  test("a stored thinking block without text renders when the session is opened", async ({ tui, backend }) => {
    const { session } = await api<{ session: { id: string } }>(backend, "POST", "/v1/sessions", {
      agent: "hya-main",
      model: "fake/model",
      workdir: backend.dir,
    })
    const { turn } = await api<{ turn: { id: string } }>(backend, "POST", `/v1/sessions/${session.id}/turns`, {
      prompt: { text: "think silently" },
    })
    await api(backend, "POST", `/v1/sessions/${session.id}/turns/${turn.id}/wait`, { timeoutMs: 20_000 })

    const term = await tui([...hyaTui(backend), "--session", session.id])
    await term.waitForText("Answer after silent thinking.", 20_000)
    await term.waitForText("Thinking · 0 words")
    expect(await term.find("Refresh failed")).toBeNull()
  })
})
