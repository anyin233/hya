import { readFile, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, expect, hyaTui, test, textStep, showStatusView } from "./hya"

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
  const screen = await term.screenBox()
  const size = await term.size()
  await term.mouse.click(screen.x + ((position.col + 0.5) / size.cols) * screen.width, screen.y + ((position.row + 0.5) / size.rows) * screen.height)
}

test("passive panes preserve focus on click and are skipped by navigation and rotation", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { viewport: { width: 1500, height: 640 } })
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("draft stays")
  await command(term, "/layout focus pane-1")
  await term.type(" in editor") // Viewer and activity above the editor cannot take focus.
  await term.waitForText("draft stays in editor")
  await term.press("Alt+ArrowLeft")
  await expect.poll(() => focus(term, "Sessions")).toBe("#73c8e8")
  for (const title of ["Todos", "Context", "No messages yet"]) {
    await click(term, title)
    await expect.poll(() => focus(term, "Sessions")).toBe("#73c8e8")
  }
  await term.type("not the draft")
  await term.paste("not pasted either")
  await command(term, "/layout focus pane-4") // Passive id cannot take focus.
  await expect.poll(() => focus(term, "Sessions")).toBe("#73c8e8")
  await command(term, "/layout focus previous")
  await expect.poll(() => focus(term, "Projects")).toBe("#73c8e8")
  await command(term, "/layout focus pane-1")
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
    await command(term, "/layout split left status")
    await term.waitForText("status · pane-8")
    await term.press("Alt+ArrowRight")
    await term.type("editor still owns this")
    await term.waitForText("editor still owns this")
    const status = (await term.find("status · pane-8"))!
    expect((await term.cell(status.row, status.col - 1))?.char).not.toMatch(/[┌─│]/)
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
    await expect.poll(async () => JSON.parse(await readFile(prefs, "utf8")).paneLayout.version).toBe(4)
    const saved = JSON.parse(await readFile(prefs, "utf8")).paneLayout
    expect(saved.active).toBe("pane-1")
    expect(JSON.stringify(saved.root)).toContain('"kind":"activity"')
    await term.attach(testInfo, "migrated-viewer-editor")
  })
})


test("pane boxes are reserved for selectable panes; Todos and Context stay borderless", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { viewport: { width: 1500, height: 640 } })
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.waitForText("Context")
  const sessions = (await term.find("Sessions"))!
  expect((await term.cell(sessions.row, sessions.col - 2))?.char).toBe("┌")
  const todos = (await term.find("Todos"))!
  const context = (await term.find("Context"))!
  for (const title of [todos, context]) expect((await term.cell(title.row, title.col - 1))?.char).not.toMatch(/[┌─│]/)
  // Scan the whole passive stack, including its former vertical and bottom borders.
  const lines = await term.lines()
  const right = sessions.col - 2
  for (const line of lines.slice(todos.row)) expect(line.slice(right)).not.toMatch(/[┌┐└┘│─]/)
  await term.press("Alt+ArrowLeft")
  await expect.poll(() => focus(term, "Sessions")).toBe("#73c8e8")
  await click(term, "Todos")
  await expect.poll(() => focus(term, "Sessions")).toBe("#73c8e8")
  await term.attach(testInfo, "borderless-passive-wide")
})

for (const width of [1100, 690]) {
  test(`non-chat output in the passive viewer has no enclosing box (${width}px)`, async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    await showStatusView(term)
    await term.waitForText("Status")
    const title = (await term.find("Status"))!
    expect((await term.cell(title.row, title.col - 1))?.char).not.toMatch(/[┌─│]/)
    const input = (await term.find("Message, !shell, or @file · / commands"))!
    for (const line of (await term.lines()).slice(0, input.row - 1)) expect(line).not.toMatch(/[┌┐└┘│─]/)
    await term.type("editor remains active")
    await term.waitForText("editor remains active")
    await term.attach(testInfo, `borderless-passive-view-${width}`)
  })
}
