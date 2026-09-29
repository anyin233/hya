import { expect, hyaTui, test } from "./hya"

for (const width of [1100, 700]) {
  test(`input has no layout status or footer around it (${width}px)`, async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Connected to hya")
    await term.type("/layout show")
    await term.press("Enter")
    await term.waitForText("Layout · 5 panes")
    const status = (await term.find("Layout · 5 panes"))!
    const header = (await term.find("hya ·"))!
    expect(status.row).toBe(header.row + 2)
    expect(await term.find("Alt+arrows select pane")).toBeNull()
    expect(await term.find("Enter a prompt · /new creates a session")).toBeNull()
    await term.type("clean input draft")
    await term.waitForText("clean input draft")
    const draft = (await term.find("clean input draft"))!
    const { rows } = await term.size()
    expect(draft.row).toBe(rows - 2)
    await term.resize(width === 1100 ? 700 : 1100, 640)
    await term.waitForText("clean input draft")
    expect((await term.find("Layout · 5 panes"))!.row).toBe(2)
    await term.press("Control+x")
    await term.type("/layout assign invalid")
    await term.press("Enter")
    await term.waitForText("Usage: /layout assign")
    expect((await term.find("Usage: /layout assign"))!.row).toBe(2)
    await term.press("Control+c")
    await term.waitForText("Press Ctrl+C again to quit")
  })
}
