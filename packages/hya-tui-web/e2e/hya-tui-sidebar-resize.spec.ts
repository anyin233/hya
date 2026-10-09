// The right sidebar's width (docs/tui.md "Sidebar" and "Tiled workspace"): hidden below 150
// columns, never narrower than 29 columns, and resized by dragging its left
// border with the mouse; the width is saved with the layout.

import type { Tui } from "./harness"
import { expect, hyaTui, test, wideViewport as wide } from "./hya"

const sidebarMinColumns = 29

async function cellPoint(term: Tui, row: number, col: number): Promise<{ x: number; y: number }> {
  const box = await term.screenBox()
  const { cols, rows } = await term.size()
  return { x: box.x + ((col + 0.5) / cols) * box.width, y: box.y + ((row + 0.5) / rows) * box.height }
}

/** The right sidebar's left border: the `┌` of the Sessions box title row. */
async function sidebarBorder(term: Tui): Promise<{ row: number; col: number }> {
  const title = (await term.find("Sessions"))!
  const line = (await term.lines())[title.row]!
  return { row: title.row, col: line.lastIndexOf("┌", title.col) }
}

async function drag(term: Tui, from: { row: number; col: number }, toCol: number): Promise<void> {
  const start = await cellPoint(term, from.row, from.col)
  const end = await cellPoint(term, from.row, toCol)
  await term.mouse.move(start.x, start.y)
  await term.mouse.down()
  await term.mouse.move((start.x + end.x) / 2, end.y, { steps: 4 })
  await term.mouse.move(end.x, end.y, { steps: 4 })
  await term.mouse.up()
}

test.describe("right sidebar width", () => {
  test("below 150 columns the sidebar stays closed without conversation metadata", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    expect((await term.size()).cols).toBeLessThan(150)
    expect(await term.find("mode manual")).toBeNull()
    expect(await term.find("Sessions")).toBeNull()
    await term.press("Control+x")
    await term.type("/sidebar")
    await term.press("Enter")
    await expect.poll(() => term.find("Commands")).toBeNull()
    expect(await term.find("Sessions")).toBeNull()
  })

  test("dragging its border resizes it, down to the minimum, and a new TUI keeps the width", async ({ tui, backend }, testInfo) => {
    let term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.waitForText("Sessions")
    const { cols } = await term.size()
    expect(cols).toBeGreaterThanOrEqual(150)
    // The default 12% share is under the minimum at this width: the sidebar is exactly 29 columns.
    const start = await sidebarBorder(term)
    expect(cols - start.col).toBe(sidebarMinColumns)

    // Drag the border 25 columns left: the sidebar grows by 25.
    await drag(term, { row: start.row + 1, col: start.col }, start.col - 25)
    await expect.poll(async () => (await sidebarBorder(term)).col).toBe(start.col - 25)
    await term.attach(testInfo, "widened")

    // Drag it to the right edge: it stops at the minimum width.
    const widened = await sidebarBorder(term)
    await drag(term, { row: widened.row + 1, col: widened.col }, cols - 3)
    await expect.poll(async () => cols - (await sidebarBorder(term)).col).toBe(sidebarMinColumns)

    // Widen it again and restart: the saved layout keeps the width.
    const narrowest = await sidebarBorder(term)
    await drag(term, { row: narrowest.row + 1, col: narrowest.col }, narrowest.col - 30)
    await expect.poll(async () => (await sidebarBorder(term)).col).toBe(narrowest.col - 30)
    term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Sessions")
    await expect.poll(async () => (await sidebarBorder(term)).col).toBe(narrowest.col - 30)
  })
})
