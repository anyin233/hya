import { expect, fixture, test } from "./harness"

test.describe("OpenTUI in the browser", () => {
  test("renders borders, title, and truecolor text", async ({ tui }) => {
    const term = await tui(fixture("opentui-probe.ts"))
    await term.waitForText("probe")
    await term.waitForText("accent")
    const corner = await term.cell(0, 0)
    expect(corner?.char).toMatch(/[┌╭+]/)
    const accent = (await term.find("accent"))!
    const cell = await term.cell(accent.row, accent.col)
    expect(cell?.fg).toBe("#73c8e8")
  })

  test("lays out wide glyphs as two columns", async ({ tui }) => {
    const term = await tui(fixture("opentui-probe.ts"))
    await term.waitForText("wide:")
    const start = (await term.find("wide:"))!
    const first = await term.cell(start.row, start.col + 5)
    expect(first).toMatchObject({ char: "你", width: 2 })
    expect(await term.cell(start.row, start.col + 7)).toMatchObject({ char: "好", width: 2 })
    expect(await term.cell(start.row, start.col + 9)).toMatchObject({ char: "|" })
  })

  test("delivers keystrokes and Enter to the focused input", async ({ tui }) => {
    const term = await tui(fixture("opentui-probe.ts"))
    await term.waitForText("type here")
    await term.type("hello web")
    await term.press("Enter")
    await term.waitForText("echo:hello web")
  })

  test("propagates browser resizes to the TUI", async ({ tui }) => {
    const term = await tui(fixture("opentui-probe.ts"), { viewport: { width: 900, height: 500 } })
    const initial = await term.size()
    await term.waitForText(`size ${initial.cols}x${initial.rows}`)
    const resized = await term.resize(1300, 760)
    expect(resized.cols).toBeGreaterThan(initial.cols)
    await term.waitForText(`size ${resized.cols}x${resized.rows}`)
  })

  test("Ctrl+C exits the TUI cleanly", async ({ tui }) => {
    const term = await tui(fixture("opentui-probe.ts"))
    await term.waitForText("probe")
    await term.press("Control+C")
    expect(await term.waitForExit()).toBe(0)
  })
})
