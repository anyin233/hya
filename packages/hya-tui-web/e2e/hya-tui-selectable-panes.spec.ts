import { readFile, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, expect, hyaTui, test, textStep } from "./hya"

async function command(term: Tui, text: string) {
  await term.press("Control+x")
  await term.type(text)
  await term.press("Enter")
  await expect.poll(() => term.find("Commands")).toBeNull()
}
async function focus(term: Tui, title: string) {
  const position = (await term.find(title))!
  return (await term.cell(position.row, position.col - 1))?.fg
}
async function click(term: Tui, title: string) {
  const position = (await term.find(title))!
  const screen = (await term.page.locator(".xterm-screen").boundingBox())!
  const size = await term.size()
  await term.page.mouse.click(screen.x + ((position.col + 0.5) / size.cols) * screen.width, screen.y + ((position.row + 0.5) / size.rows) * screen.height)
}

test("passive panes preserve focus on click and are skipped by navigation and rotation", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { viewport: { width: 1500, height: 640 } })
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("draft stays")
  await term.press("Alt+ArrowUp")
  await term.type(" in editor") // Viewer and activity above the editor cannot take focus.
  await term.waitForText("draft stays in editor")
  await term.press("Alt+ArrowRight")
  await expect.poll(() => focus(term, "Sessions")).toBe("#73c8e8")
  for (const title of ["Todos", "Context", "No messages yet"]) {
    await click(term, title)
    await expect.poll(() => focus(term, "Sessions")).toBe("#73c8e8")
  }
  await term.type("not the draft")
  await term.page.evaluate(() => window.hyaTerm.term.paste("not pasted either"))
  await command(term, "/layout focus pane-4") // Passive id cannot take focus.
  await expect.poll(() => focus(term, "Sessions")).toBe("#73c8e8")
  await command(term, "/layout focus next")
  await expect.poll(() => focus(term, "Projects")).toBe("#73c8e8")
  await command(term, "/layout focus next")
  await term.type(" continues")
  await term.waitForText("draft stays in editor continues")
  expect(await term.text()).not.toContain("not the draft")
  expect(await term.text()).not.toContain("not pasted either")
  await term.attach(testInfo, "selectable-passive-wide")
})

for (const width of [1100, 690]) {
  test(`adding passive Status keeps the editor active (${width}px)`, async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    await command(term, "/layout split vertical status")
    await term.waitForText("status · pane-8")
    await term.press("Alt+ArrowRight")
    await term.type("editor still owns this")
    await term.waitForText("editor still owns this")
    const status = (await term.find("status · pane-8"))!
    expect((await term.cell(status.row, status.col - 1))?.fg).toBe("#405366")
    await term.attach(testInfo, `selectable-passive-${width}`)
  })
}


test.describe("legacy pane layout", () => {
  test.use({ model: { steps: [textStep("streaming migration check ".repeat(12) + "MIGRATION DONE", { chunkSize: 3, delayMs: 40 })] } })
  test("old conversation id still edits and migrated layout keeps the activity indicator", async ({ tui, backend }, testInfo) => {
    const prefs = join(backendConfigDir(backend), "tui.json")
    await writeFile(prefs, JSON.stringify({ paneLayout: { version: 2, active: "pane-1", root: { type: "pane", id: "pane-1", kind: "conversation" } } }))
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.press("Alt+ArrowUp")
    await term.type("migration hello")
    await term.press("Enter")
    await term.waitForText(/Writing|Thinking/, 20_000)
    await term.waitForText("MIGRATION DONE", 20_000)
    await command(term, "/layout focus next")
    await expect.poll(async () => JSON.parse(await readFile(prefs, "utf8")).paneLayout.version).toBe(3)
    const saved = JSON.parse(await readFile(prefs, "utf8")).paneLayout
    expect(saved.active).toBe("pane-1")
    expect(JSON.stringify(saved.root)).toContain('"kind":"activity"')
    await term.attach(testInfo, "migrated-viewer-editor")
  })
})
