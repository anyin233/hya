// Copy (docs/tui.md "Copy"): `/copy` and a mouse selection in the transcript
// write the text to the terminal as an OSC 52 clipboard sequence. The spec
// registers an OSC 52 handler on the page's xterm.js terminal
// (`window.hyaTerm.term`, the generic test hook of docs/tui-web.md) and
// decodes what reached it, so it checks the bytes the TUI emitted, not the
// browser's clipboard permissions.

import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep } from "./hya"

declare global {
  interface Window {
    osc52?: string[]
  }
}

/** Record every OSC 52 payload (`c;<base64>`) the terminal receives from now on. */
async function captureOsc52(term: Tui): Promise<void> {
  await term.page.evaluate(() => {
    window.osc52 = []
    window.hyaTerm.term.parser.registerOscHandler(52, (data: string) => {
      window.osc52!.push(data)
      return true
    })
  })
}

/** The decoded texts of the OSC 52 writes seen so far. */
async function copied(term: Tui): Promise<string[]> {
  const payloads = await term.page.evaluate(() => window.osc52 ?? [])
  return payloads.map((payload) => Buffer.from(payload.slice(payload.indexOf(";") + 1), "base64").toString("utf8"))
}

/** Screen pixel of the middle of cell (row, col). */
async function cellPoint(term: Tui, row: number, col: number): Promise<{ x: number; y: number }> {
  const box = (await term.page.locator(".xterm-screen").boundingBox())!
  const { cols, rows } = await term.size()
  return { x: box.x + ((col + 0.5) / cols) * box.width, y: box.y + ((row + 0.5) / rows) * box.height }
}

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

test.describe("copy", () => {
  test.use({ model: { steps: [textStep("Alpha bravo charlie delta echo.")] } })

  test("/copy sends the last reply over OSC 52 and says how many characters", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await captureOsc52(term)
    await prompt(term, "/copy")
    await term.waitForText("Nothing to copy: no assistant reply yet")
    expect(await copied(term)).toEqual([])

    await prompt(term, "say it")
    await term.waitForText("Alpha bravo charlie delta echo.", 20_000)
    await prompt(term, "/copy")
    await term.waitForText("Copied 31 chars")
    await expect.poll(() => copied(term)).toEqual(["Alpha bravo charlie delta echo."])
  })

  test("dragging over transcript text highlights it in the theme's selection color and copies it on release", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "say it")
    await term.waitForText("Alpha bravo charlie delta echo.", 20_000)
    await captureOsc52(term)

    const at = (await term.find("bravo charlie"))!
    const start = await cellPoint(term, at.row, at.col)
    const end = await cellPoint(term, at.row, at.col + "bravo charlie".length - 1)
    await term.page.mouse.move(start.x, start.y)
    await term.page.mouse.down()
    await term.page.mouse.move((start.x + end.x) / 2, end.y, { steps: 3 })
    await term.page.mouse.move(end.x, end.y, { steps: 3 })
    // While dragging, the selected cells use the hya theme's selection background.
    await expect.poll(async () => (await term.cell(at.row, at.col + 2))?.bg).toBe("#2f4d6b")
    await term.attach(testInfo, "selecting")
    await term.page.mouse.up()

    await term.waitForText("Copied 13 chars")
    await expect.poll(() => copied(term)).toEqual(["bravo charlie"])
    // The text keeps its own color under the highlight.
    expect((await term.cell(at.row, at.col + 2))?.fg).not.toBe("#2f4d6b")
  })

  test("a plain click copies nothing", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "say it")
    await term.waitForText("Alpha bravo charlie delta echo.", 20_000)
    await captureOsc52(term)
    const at = (await term.find("bravo"))!
    const point = await cellPoint(term, at.row, at.col)
    await term.page.mouse.click(point.x, point.y)
    await prompt(term, "/status")
    await term.waitForText("Server")
    expect(await copied(term)).toEqual([])
  })
})
