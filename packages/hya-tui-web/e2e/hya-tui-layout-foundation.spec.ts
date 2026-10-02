import { readFile, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, expect, hyaTui, statusSessionId, test, textStep } from "./hya"
const leaf = (id: number, kind: string) => ({ type: "pane", id: `pane-${id}`, kind })
const weight = (node: unknown, value = 1) => ({ node, size: { mode: "weight", value } })
const content = (node: unknown) => ({ node, size: { mode: "content" } })
async function command(term: Tui, text: string) {
  await term.press("Control+x")
  await term.type(text)
  await term.press("Enter")
  await expect.poll(() => term.find("Commands")).toBeNull()
}
async function focused(term: Tui, kind: string, id: number) {
  if (kind !== "composer") return !!await term.find(`▸ ${kind} · pane-${id}`)
  const input = await term.find("Message, !shell")
  return !!input && (await term.cell(input.row - 1, input.col))?.fg === "#73c8e8"
}
for (const width of [1100, 690]) {
  test(`horizontal cycle reaches every visible selectable pane; vertical movement uses drawn bounds (${width}px)`, async ({ tui, backend }, testInfo) => {
    const paneLayout = { version: 4, active: "pane-1", root: { type: "split", id: "group-1", direction: "row", children: [
      weight({ type: "split", id: "group-2", direction: "column", children: [weight(leaf(8, "jobs")), weight(leaf(9, "models"))] }),
      weight({ type: "split", id: "group-3", direction: "column", children: [weight(leaf(10, "workflows")), weight(leaf(6, "conversation")), content(leaf(1, "composer"))] }),
    ] } }
    await writeFile(join(backendConfigDir(backend), "tui.json"), JSON.stringify({ paneLayout }))
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell")
    await statusSessionId(term)
    for (const [kind, id] of [["jobs", 8], ["workflows", 10], ["models", 9], ["composer", 1]] as const) {
      await term.press("Alt+ArrowRight")
      await expect.poll(() => focused(term, kind, id)).toBe(true)
    }
    await term.press("Alt+ArrowLeft")
    await expect.poll(() => focused(term, "models", 9)).toBe(true)
    await term.press("Alt+ArrowUp")
    await expect.poll(() => focused(term, "jobs", 8)).toBe(true)
    await term.press("Alt+ArrowDown")
    await expect.poll(() => focused(term, "models", 9)).toBe(true)
    await command(term, "/layout focus pane-1")
    await term.press("Alt+ArrowUp")
    await expect.poll(() => focused(term, "workflows", 10)).toBe(true)
    await term.press("Alt+ArrowDown")
    await expect.poll(() => focused(term, "composer", 1)).toBe(true)
    await term.attach(testInfo, "reachable-panes")
  })
}

test.describe("structural operations preserve mounted panes", () => {
  test.use({ model: { steps: [textStep(Array.from({ length: 120 }, (_, i) => `scroll line ${i}`).join("\n\n") + "\n\nSTRUCTURE END")] } })
  test("insert, move, wrap and reload preserve draft, transcript scroll and session", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell")
    await statusSessionId(term)
    await term.type("long reply")
    await term.press("Enter")
    await term.waitForText("STRUCTURE END", 20_000)
    await term.waitForIdle()
    await term.press("PageUp")
    await expect.poll(() => term.find("STRUCTURE END")).toBeNull()
    await term.type("retained draft")
    await term.waitForText("retained draft")
    await command(term, "/layout insert root 1 jobs")
    await term.waitForText("jobs · pane-8")
    await term.waitForText("retained draft")
    await command(term, "/layout move pane-8 group-2 1")
    await term.waitForText("jobs · pane-8")
    await term.waitForText("retained draft")
    await command(term, "/layout wrap root column status before")
    await term.waitForText("status · pane-9")
    await term.waitForText("retained draft")
    expect(await term.find("STRUCTURE END")).toBeNull()
    await command(term, "/layout reload")
    await term.waitForText("retained draft")
    expect(await term.find("STRUCTURE END")).toBeNull()
    await command(term, "/layout tree")
    await term.waitForText("Layout tree")
    await term.waitForText("group-4 · column")
    await term.attach(testInfo, "structural-tree")
    await term.press("Escape")
    await expect.poll(() => term.find("Layout tree")).toBeNull()
    await command(term, "/layout remove pane-9")
    await expect.poll(() => term.find("status · pane-9")).toBeNull()
    await command(term, "/layout focus pane-1")
    await term.type(" continues")
    await term.waitForText("retained draft continues")
    expect(await term.find("STRUCTURE END")).toBeNull()
    await term.attach(testInfo, "structural-layout")
  })
})


test("dragging with a hidden sibling resizes the drawn pair and preserves the hidden weight", async ({ tui, backend }) => {
  const path = join(backendConfigDir(backend), "tui.json")
  const paneLayout = { version: 4, active: "pane-1", root: { type: "split", id: "group-1", direction: "row", children: [
    weight(leaf(2, "projects"), .1), weight(leaf(8, "jobs"), .45),
    weight({ type: "split", id: "group-2", direction: "column", children: [weight(leaf(6, "conversation")), content(leaf(1, "composer"))] }, .45),
  ] } }
  await writeFile(path, JSON.stringify({ paneLayout }))
  const term = await tui(hyaTui(backend))
  await term.waitForText("Message, !shell")
  await statusSessionId(term)
  expect(await term.find("Projects")).toBeNull()
  const editor = (await term.find("Message, !shell"))!
  const boundary = editor.col - 2
  const screen = (await term.page.locator(".xterm-screen").boundingBox())!
  const { cols, rows } = await term.size()
  const point = (col: number) => ({ x: screen.x + (col + .5) / cols * screen.width, y: screen.y + 1.5 / rows * screen.height })
  const start = point(boundary), end = point(boundary - 5)
  await term.page.mouse.move(start.x, start.y)
  await term.page.mouse.down()
  await term.page.mouse.move(end.x, end.y, { steps: 5 })
  await term.page.mouse.up()
  await expect.poll(async () => (await term.find("Message, !shell"))?.col).toBe(editor.col - 5)
  await expect.poll(async () => JSON.parse(await readFile(path, "utf8")).paneLayout.root.children[1].size.value).not.toBe(.45)
  expect(JSON.parse(await readFile(path, "utf8")).paneLayout.root.children[0].size.value).toBe(.1)
})

test.describe("column boundary dragging", () => {
  test.use({ model: { steps: [textStep(Array.from({ length: 80 }, (_, i) => `column line ${i}`).join("\n\n") + "\n\nCOLUMN END")] } })
  test("dragging above the input skips the zero-height activity slot", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell")
    await statusSessionId(term)
    await term.type("long reply")
    await term.press("Enter")
    await term.waitForText("COLUMN END", 20_000)
    await term.waitForIdle()
    const marker = (await term.find("COLUMN END"))!
    const editor = (await term.find("Message, !shell"))!
    const screen = (await term.page.locator(".xterm-screen").boundingBox())!
    const { cols, rows } = await term.size()
    const point = (row: number) => ({ x: screen.x + 30.5 / cols * screen.width, y: screen.y + (row + .5) / rows * screen.height })
    const start = point(editor.row - 1), end = point(editor.row - 6)
    await term.page.mouse.move(start.x, start.y)
    await term.page.mouse.down()
    await term.page.mouse.move(end.x, end.y, { steps: 5 })
    await term.page.mouse.up()
    await expect.poll(async () => (await term.find("COLUMN END"))?.row).toBe(marker.row - 5)
    const path = join(backendConfigDir(backend), "tui.json")
    await expect.poll(async () => JSON.parse(await readFile(path, "utf8")).paneLayout.root.children[1].node.children[2].size.mode).toBe("weight")
    const children = JSON.parse(await readFile(path, "utf8")).paneLayout.root.children[1].node.children
    expect(children[1].node.kind).toBe("activity")
    expect(children[1].size).toEqual({ mode: "content" })
  })
})
