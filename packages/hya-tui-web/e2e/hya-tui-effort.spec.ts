// Server-persisted /effort behavior: the explicit status view shows the
// effective effort, provider
// requests carry it, it can change while a turn runs, another client's change
// shows live, an Agent-level effort does not swallow it, and the choice
// survives a TUI restart against the same daemon/database. The narrow resize,
// picker escape, and exit paths remain covered below.

import type { Tui } from "./harness"
import { expect, hangStep, hyaTui, test, textStep, type FakeModel } from "./hya"

/** The part of a captured `/v1/responses` body the effort assertions read. */
type ResponsesBody = { model?: string; reasoning?: { effort?: string; summary?: string } }


test.describe("hya TUI /effort", () => {
  // Two of these tests launch the TUI twice (restart) or run three prompt
  // turns (reset flow); the 30s project default is too tight under load.
  test.setTimeout(60_000)

  test.use({
    model: {
      // `openai-response` advertises the kind menu: none/minimal/low/medium/
      // high/xhigh/max; no configured `reasoning.default`, so `default` means
      // "no request effort".
      protocol: "responses",
      models: ["gpt-6-astra"],
      steps: [textStep("effort reply 1"), textStep("effort reply 2"), textStep("effort reply 3"), textStep("effort reply 4")],
    },
  })

  async function prompt(term: Tui, text: string): Promise<void> {
    await term.type(text)
    await term.press("Enter")
  }

  /** Open the explicit status view and wait for its effective effort. */
  async function effortShows(term: Tui, effort: string): Promise<void> {
    const pattern = new RegExp(`Thinking\\s+${effort}(?:\\s|$)`)
    await expect.poll(async () => {
      await prompt(term, "/status")
      return pattern.test(await term.text())
    }, { timeout: 20_000 }).toBe(true)
  }

  /** The `index`th main-turn body the fake model captured (title requests are diverted, so prompts index cleanly). */
  async function capturedBody(fakeModel: FakeModel, index: number): Promise<ResponsesBody> {
    await expect.poll(() => fakeModel.requests().length, { timeout: 20_000 }).toBeGreaterThanOrEqual(index + 1)
    return fakeModel.requests()[index] as ResponsesBody
  }

  test("a fresh gpt-6-astra session sends no effort and status says default", async ({ tui, backend, fakeModel }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await effortShows(term, "default")

    await prompt(term, "say hi")
    await term.waitForText("effort reply 1", 20_000)
    const body = await capturedBody(fakeModel!, 0)
    expect(body.model).toBe("gpt-6-astra")
    // No choice anywhere: the request carries no reasoning field at all —
    // neither a synthesized maximum nor a remote-published default.
    expect(body.reasoning).toBeUndefined()
    await term.attach(testInfo, "fresh-default")
  })

  test("/effort picker: choosing low updates status and the next request body", async ({ tui, backend, fakeModel }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/effort")
    await term.waitForText("Thinking effort")
    await term.waitForText(/default\s+\[default\]/)
    await term.waitForText(/none\s+\[off\]/)
    // No choice yet: the `default` row is the current one.
    await term.waitForText(/●\s*default/)
    await term.attach(testInfo, "effort-picker")

    await term.type("low")
    await term.waitForText(/1 of \d+/)
    await term.press("Enter")
    await effortShows(term, "low")
    await prompt(term, "lower the effort")
    await term.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })

  test("/effort low directly switches the session and the request body like the picker", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/effort low")
    await effortShows(term, "low")
    await prompt(term, "hello")
    await term.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })

  test("/exit works and the remembered effort lands on the next start's new session", async ({ tui, backend, fakeModel }) => {
    const first = await tui(hyaTui(backend))
    await first.waitForText("Message, !shell, or @file · / commands")
    await prompt(first, "/effort low")
    await effortShows(first, "low")
    await prompt(first, "/exit")
    expect(await first.waitForExit(20_000)).toBe(0)

    // The same preferences file: the new start's auto-created session opens on `#low`.
    const second = await tui(hyaTui(backend))
    await second.waitForText("Message, !shell, or @file · / commands")
    await effortShows(second, "low")

    await prompt(second, "still low?")
    await second.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })

  test("/effort default removes the suffix and the request effort; explicit none sends Responses none", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/effort low")
    await effortShows(term, "low")
    await prompt(term, "warm up")
    await term.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")

    await prompt(term, "/effort default")
    await effortShows(term, "default")
    await prompt(term, "reset check")
    await term.waitForText("effort reply 2", 20_000)
    expect((await capturedBody(fakeModel!, 1)).reasoning).toBeUndefined()

    await prompt(term, "/effort none")
    await effortShows(term, "none")
    await prompt(term, "none check")
    await term.waitForText("effort reply 3", 20_000)
    // Explicit `none` is a real request value on the Responses protocol, not an omitted field.
    expect((await capturedBody(fakeModel!, 2)).reasoning?.effort).toBe("none")
  })

  test("a narrow viewport keeps the current effort visible", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/effort low")
    await effortShows(term, "low")

    const { cols } = await term.resize(690, 640)
    expect(cols).toBeGreaterThanOrEqual(78)
    expect(cols).toBeLessThanOrEqual(84)
    await effortShows(term, "low")
  })

  test.describe("during a turn", () => {
    test.use({ model: { protocol: "responses", models: ["gpt-6-astra"], steps: [hangStep(), textStep("effort reply 1")] } })

    test("/effort switches while a turn runs: the label updates at once and the next request carries it", async ({ tui, backend, fakeModel }) => {
      const term = await tui(hyaTui(backend))
      await term.waitForText("Message, !shell, or @file · / commands")
      await prompt(term, "long task")
      await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)

      await prompt(term, "/effort max")
      await effortShows(term, "max")
      expect((await capturedBody(fakeModel!, 0)).reasoning).toBeUndefined()
      fakeModel!.release()
      // A prompt sent while the finishing turn still runs queues behind it.
      await prompt(term, "next turn")
      await term.waitForText("effort reply 1", 20_000)
      expect((await capturedBody(fakeModel!, 1)).reasoning?.effort).toBe("max")
    })
  })

  test("another client's effort choice shows live without any key press", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    // One finished turn: the startup reads are all done before the other client acts.
    await prompt(term, "settle")
    await term.waitForText("effort reply 1", 20_000)
    await effortShows(term, "default")

    // Another v1 client (a second TUI, the WebUI, a script) saves the model's effort.
    const response = await fetch(`${backend.url}/v1/model-effort-preferences/fake/gpt-6-astra`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ effort: "high" }) })
    expect(response.ok).toBe(true)
    await effortShows(term, "high")
  })

  test("an Agent-level effort does not swallow /effort: the switch applies and survives a restart", async ({ tui, backend, fakeModel }) => {
    const response = await fetch(`${backend.url}/v1/agent-efforts/hya-main`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ effort: "high" }) })
    expect(response.ok).toBe(true)
    const first = await tui(hyaTui(backend))
    await first.waitForText("Message, !shell, or @file · / commands")
    await effortShows(first, "high")

    await prompt(first, "/effort low")
    await effortShows(first, "low")
    await prompt(first, "/exit")
    expect(await first.waitForExit(20_000)).toBe(0)

    const second = await tui(hyaTui(backend))
    await second.waitForText("Message, !shell, or @file · / commands")
    await effortShows(second, "low")
    await prompt(second, "agent check")
    await second.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })

  test("Esc closes the effort picker and keeps the current choice", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/effort low")
    await effortShows(term, "low")

    await prompt(term, "/effort")
    await term.waitForText("Filter")
    await term.press("Escape")
    await expect.poll(async () => term.text()).not.toContain("Filter")

    await prompt(term, "unchanged check")
    await term.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })
})
