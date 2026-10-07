import { readFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, expect, hyaTui, statusSessionId, test } from "./hya"

async function command(term: Tui, text: string) {
  await term.press("Control+x"); await term.type(text); await term.press("Enter")
  await expect.poll(() => term.find("Commands")).toBeNull()
}
async function click(term: Tui, text: string) {
  await term.waitForText(text)
  const at = (await term.find(text))!
  const box = (await term.page.locator(".xterm-screen").boundingBox())!
  const { cols, rows } = await term.size()
  await term.page.mouse.click(box.x + (at.col + .5) / cols * box.width, box.y + (at.row + .5) / rows * box.height)
}
function flatten(node: any): any[] { return node.type === "pane" ? [node] : [node, ...node.children.flatMap((child: any) => flatten(child.node))] }

for (const width of [1100, 690]) {
  test(`direct keys mark, wrap, insert and remove without touching the draft (${width}px)`, async ({ tui, backend }, testInfo) => {
    const saved = async () => JSON.parse(await readFile(join(backendConfigDir(backend), "tui.json"), "utf8")).paneLayout
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 }, hostArgs: ["--shift-enter-lf"] })
    await term.waitForText("Message, !shell")
    await statusSessionId(term)
    await term.type("keep my draft")
    await command(term, "/layout tree")
    await click(term, "pane-5 context")
    await term.press("Shift+Enter")
    await term.waitForText("Marked pane-5")
    await term.attach(testInfo, "marked-target")
    await click(term, "pane-4 todos")
    await term.press("w")
    await term.waitForText("r row · c column")
    await term.press("r")
    await term.waitForText("Wrap pane-5 in row")
    await term.waitForText("new pane after")
    await term.attach(testInfo, "wrap-preview")
    await click(term, "jobs")
    await term.waitForText("pane-9 jobs")
    await expect.poll(async () => {
      const parent = flatten((await saved()).root).find((node) => node.type === "split" && node.children.some((child: any) => child.node.id === "pane-9"))
      return { direction: parent?.direction, children: parent?.children.map((child: any) => child.node.id) }
    }).toEqual({ direction: "row", children: ["pane-5", "pane-9"] })
    await term.press("Space") // Mark newly inserted node, replacing the previous mark.
    await term.waitForText("Marked pane-9")
    await term.press("Home") // Cursor is elsewhere; remove still uses the mark.
    await term.press("Backspace")
    await expect.poll(() => term.find("pane-9 jobs")).toBeNull()

    await click(term, "pane-4 todos")
    await term.press("i")
    await term.waitForText("Insert here · before pane-4")
    await click(term, "status")
    await term.waitForText("pane-9 status")
    await expect.poll(async () => flatten((await saved()).root).find((node) => node.id === "group-3").children.map((child: any) => child.node.id)).toEqual(["pane-3", "pane-9", "pane-4", "pane-5"])
    await term.press("Delete")
    await expect.poll(() => term.find("pane-9 status")).toBeNull()

    await click(term, "group-3 column")
    await term.press("Shift+Enter")
    await term.waitForText("Marked group-3")
    await term.press("Home")
    await term.press("Delete")
    await term.waitForText("Remove group-3?")
    await term.press("Enter") // Cancel is the default.
    await expect.poll(() => term.find("Remove group-3?")).toBeNull()
    expect(flatten((await saved()).root).some((node) => node.id === "group-3")).toBe(true)
    await term.press("Backspace")
    await term.waitForText("Remove group-3?")
    await term.press("ArrowDown"); await term.press("Enter")
    await expect.poll(() => term.find("group-3 column")).toBeNull()
    await click(term, "pane-1 composer")
    await term.press("Delete")
    await term.waitForText("Cannot remove")
    await term.press("Escape")
    await term.attach(testInfo, "direct-layout-keys")
    await command(term, "/layout close layout")
    await term.type(" intact")
    await term.waitForText("keep my draft intact")
    await command(term, "/exit")
    expect(await term.waitForExit()).toBe(0)
  })
}

test("root insertion and column wrap offer position/cancel controls and preserve the mark across pane switches", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { hostArgs: ["--shift-enter-lf"] })
  await term.waitForText("Message, !shell")
  await statusSessionId(term)
  await command(term, "/layout tree")
  await term.press("Home"); await term.press("i")
  await term.waitForText("Insert in group-4")
  await term.press("End"); await term.press("Enter")
  await term.waitForText("Insert here")
  await click(term, "status")
  await term.waitForText("pane-9 status")
  await term.press("Shift+Enter")
  await term.waitForText("Marked pane-9")
  await term.press("Alt+ArrowRight")
  await command(term, "/layout tree")
  await term.waitForText("Marked pane-9")
  await term.press("w"); await term.press("c")
  await term.waitForText("Wrap pane-9 in column")
  await term.press("Tab")
  await term.waitForText("new pane before")
  await term.press("Escape")
  await expect.poll(() => term.find("Choose pane job")).toBeNull()
  await term.press("Escape")
  await expect.poll(() => term.find("Marked pane-9")).toBeNull()
  await term.press("w"); await term.press("c")
  await click(term, "jobs")
  await term.waitForText("pane-10 jobs")
  await term.resize(690, 640)
  await term.waitForText("i insert")
  await term.waitForText("Enter actions")
  await term.attach(testInfo, "column-wrap-narrow")
})

test("direct editing keys can be disabled through the visible keybinding settings", async ({ tui, backend }) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText("Message, !shell")
  await statusSessionId(term)
  await command(term, "/layout tree")
  await click(term, "pane-5 context")
  await command(term, "/keybind unset I")
  await term.waitForText("Keybind unset")
  await term.press("Escape")
  await expect.poll(() => term.find("Keybind unset")).toBeNull()
  await term.press("i")
  expect(await term.find("Choose pane job")).toBeNull()
  expect(await term.find("Insert here")).toBeNull()
  await command(term, "/keybind reset I")
  await term.waitForText("Keybind reset")
  await term.press("Escape")
  await expect.poll(() => term.find("Keybind reset")).toBeNull()
  await term.press("i")
  await term.waitForText("Insert here · before pane-5")
  await term.press("Backspace"); await term.press("Delete") // Chooser owns keys; neither removes the target.
  await term.waitForText("Insert here · before pane-5")
  await term.press("Escape")
  await expect.poll(() => term.find("Choose pane job")).toBeNull()
  await term.waitForText("pane-5 context")
})
