import { expect, hyaTui, test } from "./hya"

for (const width of [1100, 700]) {
  test(`conversation has no heading or status rows (${width}px)`, async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("/layout show")
    await term.press("Enter")
    await expect.poll(() => term.find("Commands")).toBeNull()
    await term.waitForText("No messages yet. Type a prompt below.")
    // The compact context line (docs/tui.md "Layout") appears once the backend is ready; it is the
    // only row above the transcript, followed by one blank row.
    // Startup re-renders the conversation while it settles: read the rows once they hold.
    await expect.poll(async () => (await term.find("mode manual"))?.row).toBe(0)
    await expect.poll(async () => (await term.find("No messages yet. Type a prompt below."))?.row).toBe(2)
    expect(await term.find("hya ·")).toBeNull()
    expect(await term.find("Layout · 5 panes")).toBeNull()
    expect(await term.find("Alt+arrows select pane")).toBeNull()
    expect(await term.find("Enter a prompt · /new creates a session")).toBeNull()
    await term.type("clean input draft")
    await term.waitForText("clean input draft")
    const draft = (await term.find("clean input draft"))!
    const { rows } = await term.size()
    expect(draft.row).toBe(rows - 2)
    await term.resize(width === 1100 ? 700 : 1100, 640)
    await term.waitForText("clean input draft")
    expect((await term.find("No messages yet. Type a prompt below."))!.row).toBeLessThanOrEqual(2)
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
    expect(await term.find("mode manual")).not.toBeNull()
  })
}
