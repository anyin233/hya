// Server-persisted /effort behavior: the header shows effective effort and
// source, provider requests carry it, and the preference survives a TUI
// restart against the same daemon/database. No TUI preferences cache is used.
// The narrow resize, picker escape, and exit paths remain covered below.
 
import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep, type FakeModel } from "./hya"

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

  /** The header is the screen's first row; the transcript may still show older `#variant` refs below it, so effort display is polled on row 0 only. */
  async function headerShows(term: Tui, needle: string): Promise<void> {
    await expect.poll(async () => (await term.lines())[0] ?? "").toContain(needle)
  }

  /** The header line settled on not containing `needle`. */
  async function headerLacks(term: Tui, needle: string): Promise<void> {
    await expect.poll(async () => (await term.lines())[0] ?? "").not.toContain(needle)
  }

  /** The `index`th main-turn body the fake model captured (title requests are diverted, so prompts index cleanly). */
  async function capturedBody(fakeModel: FakeModel, index: number): Promise<ResponsesBody> {
    await expect.poll(() => fakeModel.requests().length, { timeout: 20_000 }).toBeGreaterThanOrEqual(index + 1)
    return fakeModel.requests()[index] as ResponsesBody
  }

  test("a fresh gpt-6-astra session sends no effort and the header says default", async ({ tui, backend, fakeModel }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await headerShows(term, "thinking default")

    await prompt(term, "say hi")
    await term.waitForText("effort reply 1", 20_000)
    const body = await capturedBody(fakeModel!, 0)
    expect(body.model).toBe("gpt-6-astra")
    // No choice anywhere: the request carries no reasoning field at all —
    // neither a synthesized maximum nor a remote-published default.
    expect(body.reasoning).toBeUndefined()
    await term.attach(testInfo, "fresh-default")
  })

  test("/effort picker: choosing low updates the header and the next request body", async ({ tui, backend, fakeModel }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
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
    await term.waitForText("Thinking effort → low")
    await headerShows(term, "thinking low (pref)")
    await prompt(term, "lower the effort")
    await term.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })

  test("/effort low directly switches the session and the request body like the picker", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/effort low")
    await term.waitForText("Thinking effort → low")
    await headerShows(term, "thinking low (pref)")
    await prompt(term, "hello")
    await term.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })

  test("/exit works and the remembered effort lands on the next start's new session", async ({ tui, backend, fakeModel }) => {
    const first = await tui(hyaTui(backend))
    await first.waitForText("Connected to hya")
    await prompt(first, "/effort low")
    await first.waitForText("Thinking effort → low")
    await prompt(first, "/exit")
    expect(await first.waitForExit(20_000)).toBe(0)

    // The same preferences file: the new start's auto-created session opens on `#low`.
    const second = await tui(hyaTui(backend))
    await second.waitForText("Connected to hya")
    await headerShows(second, "thinking low (pref)")

    await prompt(second, "still low?")
    await second.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })

  test("/effort default removes the suffix and the request effort; explicit none sends Responses none", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/effort low")
    await term.waitForText("Thinking effort → low")
    await prompt(term, "warm up")
    await term.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")

    await prompt(term, "/effort default")
    await term.waitForText("Thinking effort → default")
    await headerShows(term, "thinking default")
    await headerShows(term, "thinking default")
    await prompt(term, "reset check")
    await term.waitForText("effort reply 2", 20_000)
    expect((await capturedBody(fakeModel!, 1)).reasoning).toBeUndefined()

    await prompt(term, "/effort none")
    await term.waitForText("Thinking effort → none")
    await prompt(term, "none check")
    await term.waitForText("effort reply 3", 20_000)
    // Explicit `none` is a real request value on the Responses protocol, not an omitted field.
    expect((await capturedBody(fakeModel!, 2)).reasoning?.effort).toBe("none")
  })

  test("a narrow viewport keeps the current effort visible", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/effort low")
    await term.waitForText("Thinking effort → low")

    const { cols } = await term.resize(690, 640)
    expect(cols).toBeGreaterThanOrEqual(78)
    expect(cols).toBeLessThanOrEqual(84)
    await headerShows(term, "thinking low (pref)")
  })

  test("Esc closes the effort picker and keeps the current choice", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/effort low")
    await term.waitForText("Thinking effort → low")

    await prompt(term, "/effort")
    await term.waitForText("Filter")
    await term.press("Escape")
    await expect.poll(async () => term.text()).not.toContain("Filter")

    await prompt(term, "unchanged check")
    await term.waitForText("effort reply 1", 20_000)
    expect((await capturedBody(fakeModel!, 0)).reasoning?.effort).toBe("low")
  })
})
