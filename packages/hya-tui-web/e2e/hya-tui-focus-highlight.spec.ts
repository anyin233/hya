import type { Tui } from "./harness"
import { expect, hyaTui, test } from "./hya"

/** Count highlighted rectangles via their border corner, not text or pixels. */
async function focusBoxes(term: Tui): Promise<{ row: number; col: number }[]> {
  return term.inspect((terminal, state) => {
    const buffer = terminal.buffer.active
    const corners: { row: number; col: number }[] = []
    for (let row = 0; row < terminal.rows; row++) {
      const line = buffer.getLine(buffer.viewportY + row)
      for (let col = 0; col < terminal.cols; col++) {
        const cell = line?.getCell(col)
        if (cell?.getChars() === "┌" && cell.isFgRGB() && cell.getFgColor() === 0x73c8e8) corners.push({ row, col })
      }
    }
    return corners
  })
}

for (const width of [1100, 700]) {
  test(`exactly one box highlights the keyboard owner through panes, commands, help and resize (${width}px)`, async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    const oneBox = async () => { await expect.poll(async () => (await focusBoxes(term)).length).toBe(1) }
    await oneBox()
    expect((await focusBoxes(term))[0]!.row).toBeGreaterThan(20)
    await term.attach(testInfo, "focused-composer")
    await term.type("/layout split left jobs")
    await term.press("Enter")
    await term.waitForText("jobs · pane-8")
    await oneBox()
    expect((await focusBoxes(term))[0]!.row).toBeGreaterThan(20)
    await term.type("/")
    await term.waitForText("Commands")
    await oneBox()
    expect((await focusBoxes(term))[0]!.row).toBe(2)
    await term.attach(testInfo, "focused-command")
    await term.resize(width === 1100 ? 700 : 1100, 640)
    await oneBox()
    expect((await focusBoxes(term))[0]!.row).toBe(2)
    await term.press("Escape")
    await expect.poll(() => term.find("Commands")).toBeNull()
    await oneBox()
    expect((await focusBoxes(term))[0]!.row).toBeGreaterThan(20)
    await term.type("?")
    await term.waitForText("Help · keys and commands")
    await oneBox()
    expect((await focusBoxes(term))[0]!.row).toBe(2)
    await term.press("Escape")
    await expect.poll(() => term.find("Help · keys and commands")).toBeNull()
    await term.press("Alt+ArrowRight")
    await expect.poll(async () => (await focusBoxes(term))[0]?.row).toBeGreaterThan(20)
    await oneBox()
    expect((await focusBoxes(term))[0]!.row).toBeGreaterThan(20)
  })
}

test("provider form alone is highlighted over its view", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("/key")
  await term.press("Enter")
  await term.waitForText("Providers")
  await expect.poll(async () => (await focusBoxes(term)).length).toBe(1)
  await term.type("a")
  await term.waitForText("Add provider · 1/4")
  await expect.poll(async () => (await focusBoxes(term)).length).toBe(1)
  expect((await focusBoxes(term))[0]!.row).toBe(3)
  await term.attach(testInfo, "focused-provider-form")
  await term.press("Escape")
  await expect.poll(() => term.find("Add provider ·")).toBeNull()
  await expect.poll(async () => (await focusBoxes(term)).length).toBe(1)
  expect((await focusBoxes(term))[0]!.row).toBe(0)
  await term.press("Escape")
  await expect.poll(() => term.find("Providers")).toBeNull()
  await expect.poll(async () => (await focusBoxes(term)).length).toBe(1)
  expect((await focusBoxes(term))[0]!.row).toBeGreaterThan(20)
})

test("Projects and Sessions each receive the single focus border", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { viewport: { width: 1500, height: 640 } })
  await term.waitForText("Message, !shell, or @file · / commands")
  for (const [id, title] of [[2, "Projects"], [3, "Sessions"]] as const) {
    await term.type(`/layout focus pane-${id}`)
    await term.press("Enter")
    await expect.poll(() => term.find("Commands")).toBeNull()
    const position = (await term.find(title))!
    await expect.poll(() => focusBoxes(term)).toEqual([{ row: position.row, col: position.col - 2 }])
  }
  await term.attach(testInfo, "focused-context")
})
