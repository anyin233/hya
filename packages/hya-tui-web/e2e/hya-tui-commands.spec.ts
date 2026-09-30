import { expect, hyaTui, openStatus, test, wideViewport } from "./hya"

// Characterization specs: they lock the existing TUI look and command behavior
// so framework or module changes in packages/hya-tui cannot drift silently.

test.describe("hya TUI commands and look", () => {
  test("keeps the conversation free of persistent metadata rows", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    expect(await term.find("hya · ")).toBeNull()
    expect(await term.find("mode ")).toBeNull()
    expect(await term.find("Sessions")).toBeNull()
    const transcript = (await term.find("No messages yet"))!
    expect((await term.cell(transcript.row, transcript.col))?.bg).toBe("#11151b")
    const composer = (await term.find("Message, !shell, or @file · / commands"))!
    expect(composer.row).toBeGreaterThan(transcript.row)
  })

  test("/help opens the help overlay; /models and /api switch the main panel", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")

    await term.type("/help")
    await term.waitForText("/help")
    await term.press("Enter")
    await term.waitForText("Help · keys and commands")
    await term.type("/key")
    await term.waitForText(/\/key\s+\[local\]\s+Open the Provider View/)
    await term.press("Escape")
    await expect.poll(() => term.find("Help · keys and commands")).toBeNull()

    await term.type("/models")
    await term.waitForText("/models")
    await term.press("Enter")
    await term.waitForText("Models")
    await term.waitForText("hya/offline")
    await term.waitForText("Message, !shell, or @file · / commands")

    await term.type("/api")
    await term.waitForText("/api")
    await term.press("Enter")
    await term.waitForText("API commands")
    await term.waitForText("/v1/health")
    await term.waitForText("Message, !shell, or @file · / commands")
  })

  test("a prompt sent while /status shows goes back to the transcript and shows its reply", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("/status")
    await term.waitForText("/status")
    await term.press("Enter")
    await term.waitForText(/Server\s+http:\/\/127\.0\.0\.1:\d+/)
    await term.type("hello from the status page")
    await term.waitForText("hello from the status page")
    await term.press("Enter")
    // The submitted prompt is durable in the transcript; the status view is transient.
    await term.waitForText("┃ hello from the status page")
    await expect.poll(() => term.find("Server      http://")).toBeNull()
  })

  test("narrow terminals keep the sidebar hidden", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 760, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    const { cols } = await term.size()
    expect(cols).toBeLessThan(150)
    expect(cols).toBeGreaterThanOrEqual(58)
    await expect.poll(async () => (await term.text()).includes("Sessions")).toBe(false)
    await term.press("Control+b")
    await expect.poll(async () => (await term.text()).includes("Sessions")).toBe(false)
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
