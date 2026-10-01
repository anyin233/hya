import { expect, hyaTui, statusSessionId, test, wideViewport } from "./hya"

// Characterization specs: they lock the existing TUI look and command behavior
// so framework or module changes in packages/hya-tui cannot drift silently.

test.describe("hya TUI commands and look", () => {
  test("keeps the panel colors and leaves the conversation free of headings", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    expect(await term.find("hya · ")).toBeNull()

    const sessions = (await term.find("Sessions"))!
    const corner = await term.cell(sessions.row, sessions.col - 1)
    expect(corner?.fg).toBe("#405366")
    expect((await term.cell(sessions.row + 1, sessions.col))?.bg).toBe("#1c2530")

    // The transcript (no box since the single-column layout) sits on the base background.
    const transcript = (await term.find("No messages yet"))!
    expect((await term.cell(transcript.row, transcript.col))?.bg).toBe("#11151b")
    expect(await term.find("mode manual")).toBeNull()
    const composer = (await term.find("Message, !shell, or @file · / commands"))!
    expect(composer.row).toBeGreaterThan(transcript.row)
    expect(transcript.row).toBeGreaterThanOrEqual(0)
  })

  test("/help opens the help overlay; /models and /api switch the main panel", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")

    await term.type("/help")
    await term.press("Enter")
    await term.waitForText("Help · keys and commands")
    await term.type("/key")
    await term.waitForText(/\/key\s+\[local\]\s+Open the Provider View/)
    await term.press("Escape")
    await expect.poll(() => term.find("Help · keys and commands")).toBeNull()

    await term.type("/models")
    await term.press("Enter")
    await term.waitForText("Models")
    await term.waitForText("hya/offline")
    await term.waitForText("Message, !shell, or @file · / commands")

    await term.type("/api")
    await term.press("Enter")
    await term.waitForText("API commands")
    await term.waitForText("/v1/health")
    await term.waitForText("Message, !shell, or @file · / commands")
  })

  test("a prompt sent while /status shows goes back to the transcript and shows its reply", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("/status")
    await term.press("Enter")
    await term.waitForText(/Server\s+http:\/\/127\.0\.0\.1:\d+/)

    await term.type("hello from the status page")
    await term.press("Enter")
    // The offline model's reply is on screen, and the status page is gone.
    await term.waitForText("No live provider is available", 20_000)
    await expect.poll(() => term.find("Server      http://")).toBeNull()
  })

  test("narrow terminals keep sidebars and conversation metadata hidden", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 760, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    const { cols } = await term.size()
    expect(cols).toBeLessThan(150)
    expect(cols).toBeGreaterThanOrEqual(58)
    await statusSessionId(term)
    expect(await term.find("mode manual")).toBeNull()
    expect(await term.text()).not.toContain("Sessions")
    await term.press("Control+x")
    await term.type("/sidebar")
    await term.press("Enter")
    await term.waitForText("Message, !shell, or @file · / commands")
    expect(await term.text()).not.toContain("Sessions")
  })

  test("Ctrl+C twice quits the TUI", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    // The renderer no longer quits on the first Ctrl+C (see hya-tui-composer.spec.ts).
    await term.press("Control+c")
    expect(await term.page.evaluate(() => window.hyaTerm.exitCode)).toBeNull()
    await term.press("Control+c")
    expect(await term.waitForExit()).toBe(0)
  })
})
