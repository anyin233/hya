import { expect, hyaTui, test } from "./hya"

// The default preset (ADR-0036 `statusline`): nothing above the transcript,
// and the session facts in one status row below the input.
for (const width of [1100, 700]) {
  test(`conversation has no heading rows; the status row sits below the input (${width}px)`, async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("/layout show")
    await term.press("Enter")
    await expect.poll(() => term.find("Commands")).toBeNull()
    await term.waitForText("No messages yet. Type a prompt below.")
    // Session context never sits above the transcript.
    await expect.poll(async () => (await term.find("No messages yet. Type a prompt below."))?.row).toBe(1)
    expect(await term.find("hya ·")).toBeNull()
    expect(await term.find("Layout · 5 panes")).toBeNull()
    expect(await term.find("Alt+arrows select pane")).toBeNull()
    expect(await term.find("Enter a prompt · /new creates a session")).toBeNull()
    await term.type("clean input draft")
    await term.waitForText("clean input draft")
    const draft = (await term.find("clean input draft"))!
    const { rows } = await term.size()
    // The input row, its bottom border, then the status row on the last line.
    expect(draft.row).toBe(rows - 3)
    await expect.poll(async () => (await term.lines())[rows - 1]).toMatch(/^ \S.* · manual\b/)
    await term.resize(width === 1100 ? 700 : 1100, 640)
    await term.waitForText("clean input draft")
    expect((await term.find("No messages yet. Type a prompt below."))!.row).toBeLessThanOrEqual(2)
    // Layout editing belongs to the workspace preset: refused without a usage dump in the transcript.
    await term.press("Control+x")
    await term.type("/layout assign invalid")
    await term.press("Enter")
    await expect.poll(() => term.find("Commands")).toBeNull()
    expect(await term.find("Usage: /layout assign")).toBeNull()
    await term.attach(testInfo, "conversation-without-headings")
    await term.press("Enter")
    await term.waitForText("┃ clean input draft")
    await term.waitForText("No live provider is available", 20_000)
    expect(await term.find("hya ·")).toBeNull()
  })
}
