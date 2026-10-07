import { readFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, expect, hyaTui, statusSessionId, test } from "./hya"
import type { PaneLayout, PaneNode, PaneSplit } from "../../hya-tui/src/state/panes"

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
function nodes(node: PaneNode): PaneNode[] { return node.type === "pane" ? [node] : [node, ...node.children.flatMap((child) => nodes(child.node))] }
for (const width of [1100, 690]) {
  test(`bubble row and column siblings while keeping draft, mark, sizes and saved order (${width}px)`, async ({ tui, backend }, testInfo) => {
    const saved = async (): Promise<PaneLayout> => JSON.parse(await readFile(join(backendConfigDir(backend), "tui.json"), "utf8")).paneLayout
    const group = async (id: string) => nodes((await saved()).root).find((node) => node.id === id) as PaneSplit
    let term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell")
    await statusSessionId(term)
    await term.type("draft preserved")
    await command(term, "/layout tree")
    await click(term, "pane-3 sessions")
    await term.press("Space")
    await click(term, "pane-4 todos")
    const original = await group("group-3")
    await term.press("Shift+ArrowUp")
    await expect.poll(async () => (await group("group-3")).children.map((slot) => slot.node.id)).toEqual(["pane-4", "pane-3", "pane-5"])
    await term.waitForText("Marked pane-3")
    expect((await group("group-3")).children[0]).toEqual(original.children[1])
    await term.press("Shift+ArrowUp") // At the first sibling: no wraparound.
    expect((await group("group-3")).children.map((slot) => slot.node.id)).toEqual(["pane-4", "pane-3", "pane-5"])
    await term.press("Shift+ArrowDown")
    await expect.poll(async () => (await group("group-3")).children).toEqual(original.children)
    await term.press("Enter")
    await click(term, "Bubble next")
    await expect.poll(async () => (await group("group-3")).children.map((slot) => slot.node.id)).toEqual(["pane-3", "pane-5", "pane-4"])
    await click(term, "group-2 column")
    const parent = nodes((await saved()).root).find((node): node is PaneSplit => node.type === "split" && node.children.some((slot) => slot.node.id === "group-2"))!
    const oldIndex = parent.children.findIndex((slot) => slot.node.id === "group-2")
    await term.press("Shift+ArrowDown")
    await expect.poll(async () => (await group(parent.id)).children.findIndex((slot) => slot.node.id === "group-2")).toBe(oldIndex + 1)
    expect((await group(parent.id)).children[oldIndex + 1]).toEqual(parent.children[oldIndex])
    await term.waitForText("Shift+↑↓ bubble")
    await term.attach(testInfo, `bubbled-${width}`)
    await command(term, "/layout close layout")
    await term.waitForText("draft preserved")
    await command(term, "/layout tree")
    await term.waitForText("Layout tree")
    const reordered = await saved()
    term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Layout tree")
    expect((await saved()).root).toEqual(reordered.root)
    await command(term, "/layout close layout")
    await term.waitForText("Message, !shell")
  })
}
