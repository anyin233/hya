import { expect, hyaTui, test } from "./hya"

// Characterization specs: they lock the existing TUI look and command behavior
// so framework or module changes in packages/hya-tui cannot drift silently.

test.describe("hya TUI commands and look", () => {
  test("keeps the panel colors and the header in the accent color", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    const header = (await term.find("hya · "))!
    expect(header.row).toBe(0)
    expect((await term.cell(header.row, header.col))?.fg).toBe("#73c8e8")

    const sessions = (await term.find("Sessions"))!
    const corner = await term.cell(sessions.row, sessions.col - 1)
    expect(corner?.fg).toBe("#405366")
    expect((await term.cell(sessions.row + 1, sessions.col))?.bg).toBe("#1c2530")

    // The transcript (no box since the single-column layout) sits on the base background.
    const transcript = (await term.find("No messages yet"))!
    expect((await term.cell(transcript.row, transcript.col))?.bg).toBe("#11151b")

    const status = (await term.find("Connected to hya"))!
    expect((await term.cell(status.row, status.col))?.fg).toBe("#9caab9")
    const footer = (await term.find("Enter a prompt · /new creates a session"))!
    expect((await term.cell(footer.row, footer.col))?.fg).toBe("#9caab9")
    expect(footer.row).toBeGreaterThan(status.row)
  })

  test("/help, /models, and /api switch the main panel", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")

    await term.type("/help")
    await term.press("Enter")
    await term.waitForText("Help")
    await term.waitForText("/key set <provider>   Enter a key in a concealed prompt")
    await term.waitForText("Enter a prompt or choose a /command · Tab completes")

    await term.type("/models")
    await term.press("Enter")
    await term.waitForText("Models")
    await term.waitForText("hya/offline")
    await term.waitForText("Next: /model <provider/model> to switch this session · /help")

    await term.type("/api")
    await term.press("Enter")
    await term.waitForText("API commands")
    await term.waitForText("/v1/health")
    await term.waitForText("Next: /api GET /v1/health · /help for command syntax")
  })

  test("/key set conceals the key and Esc cancels the entry", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/key set openai")
    await term.press("Enter")
    await term.waitForText("Enter API key for openai · Enter saves · Esc cancels")
    await term.waitForText("Paste API key · Enter saves · Esc cancels")
    await term.type("sk-abc")
    await term.waitForText("Key: ••••••")
    expect(await term.text()).not.toContain("sk-abc")
    await term.press("Escape")
    await term.waitForText("Key entry cancelled")
    await term.waitForText("Enter a prompt · /new creates a session · /help lists commands")
    expect(await term.text()).not.toContain("Key: ")
  })

  test("narrow terminals hide the sidebar until Ctrl+B shows it", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 760, height: 640 } })
    await term.waitForText("Connected to hya")
    const { cols } = await term.size()
    expect(cols).toBeLessThan(110)
    expect(cols).toBeGreaterThanOrEqual(58)
    expect(await term.text()).not.toContain("Sessions")
    await term.press("Control+b")
    await term.waitForText("Sessions")
  })

  test("Ctrl+C twice quits the TUI", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    // The renderer no longer quits on the first Ctrl+C (see hya-tui-composer.spec.ts).
    await term.press("Control+c")
    await term.waitForText("Press Ctrl+C again to quit")
    await term.press("Control+c")
    expect(await term.waitForExit()).toBe(0)
  })
})
