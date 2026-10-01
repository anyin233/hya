import { expect, hyaTui, test, wideViewport } from "./hya"

test.describe("hya TUI in the browser", () => {
  test("connects to the backend and lays out the main column and the sidebar", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    // One main column (transcript, status, composer, footer) and the sidebar boxes;
    // the old Chat and Pending panels are gone.
    for (const title of ["Sessions", "Todos", "Context"]) await term.waitForText(title)
    await term.waitForText("No messages yet. Type a prompt below.")
    await term.waitForText("Message, !shell, or @file · / commands")
    const text = await term.text()
    expect(text).not.toContain("Chat")
    expect(text).not.toContain("Pending")
    expect(text).not.toMatch(/backend \d+\.\d+\.\d+ ≠ tui \d+\.\d+\.\d+/)
  })

  test("admits a prompt and shows the offline model's reply", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("hello from the browser")
    await term.press("Enter")
    await term.waitForText("● hya-main · hya/offline", 20_000)
    await term.waitForText("No live provider is available")
    const text = await term.text()
    // The user's prompt is a block with the accent bar, not a `user · stop` header.
    const prompt = (await term.find("┃ hello from the browser"))!
    expect((await term.cell(prompt.row, prompt.col))?.fg).toBe("#e6db74")
    expect(text).not.toContain("user · stop")
    // The offline model echoes the prompt back, so it shows up in both messages.
    expect(text.match(/hello from the browser/g)?.length ?? 0).toBeGreaterThanOrEqual(2)
    expect(text).not.toContain("hya ·")
  })

  test("Tab completes slash commands", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("/workf")
    await term.press("Tab")
    await term.waitForText("/workflow")
  })
})
