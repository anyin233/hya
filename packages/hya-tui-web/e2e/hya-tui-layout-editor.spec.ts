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
  test(`Layout pane edits passive weights, rejects invalid values, preserves the draft and persists (${width}px)`, async ({ tui, backend }, testInfo) => {
    const path = join(backendConfigDir(backend), "tui.json")
    const saved = async () => JSON.parse(await readFile(path, "utf8")).paneLayout
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell")
    await statusSessionId(term)
    await term.type("keep my draft")
    await command(term, "/layout tree")
    await term.waitForText("Layout tree · pane-8")
    await term.waitForText("pane-4 todos")
    await term.press("End") // Full tree, including the hidden Context pane.
    await term.press("Enter")
    await term.waitForText("Edit pane-5")
    await click(term, "Change weight")
    await term.waitForText("Weight for pane-5")
    await term.type("0"); await term.press("Enter")
    await term.waitForText("positive finite")
    await term.press("Backspace"); await term.type("2.5"); await term.press("Enter")
    await term.waitForText("weight 2.5")
    await expect.poll(async () => flatten((await saved()).root).find((node) => node.id === "group-3").children[2].size.value).toBe(2.5)
    expect((await saved()).active).toBe("pane-8")
    await term.type("should not reach the draft")
    await term.waitForText("keep my draft")
    expect(await term.find("should not reach")).toBeNull()
    await command(term, "/layout tree")
    expect(flatten((await saved()).root).filter((node) => node.kind === "layout")).toHaveLength(1)
    await term.attach(testInfo, "layout-editor-tree")
    await command(term, "/layout focus pane-1")
    await term.type(" intact")
    await term.waitForText("keep my draft intact")
    const restored = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await restored.waitForText("Layout tree · pane-8")
    await command(restored, "/layout tree")
    await restored.press("End")
    await restored.waitForText("weight 2.5")
  })
}

test("tree mouse controls insert, move, wrap and remove nodes while keyboard focus stays in the Layout pane", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText("Message, !shell")
  await statusSessionId(term)
  await command(term, "/layout tree")
  await click(term, "group-2 column")
  await click(term, "[Edit]")
  await click(term, "Add child")
  await click(term, "jobs")
  await term.waitForText("pane-9 jobs")
  await click(term, "pane-9 jobs")
  await term.press("Enter")
  await click(term, "Move")
  await click(term, "group-2 column")
  await click(term, "Before pane-6")
  await term.waitForText("pane-9 jobs")
  await term.press("Enter")
  await click(term, "Wrap in row")
  await click(term, "models")
  await term.waitForText("pane-10 models")
  await click(term, "pane-10 models")
  await term.press("Enter")
  await click(term, "Change job")
  await click(term, "status")
  await term.waitForText("pane-10 status")
  await term.press("Enter")
  await click(term, "Remove")
  await term.waitForText("Remove pane-10?")
  await term.press("Escape") // Confirmation is cancellable without editing the tree.
  await term.waitForText("Edit pane-10")
  await click(term, "Remove")
  await click(term, "Remove node")
  await expect.poll(() => term.find("pane-10 status")).toBeNull()
  await click(term, "pane-1 composer")
  await term.press("Enter")
  await click(term, "Remove")
  await term.waitForText("Cannot remove")
  await term.press("Escape")
  await term.attach(testInfo, "layout-editor-mutations")
  await command(term, "/layout close layout")
  await expect.poll(() => term.find("Layout tree")).toBeNull()
  await term.type("editor still works")
  await term.waitForText("editor still works")
})

test("a Layout pane added by wrap supports keyboard-only insertion and self-removal", async ({ tui, backend }) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText("Message, !shell")
  await statusSessionId(term)
  // Split the full center rather than only the three-row input dock.
  await command(term, "/layout wrap group-2 row layout before")
  await term.waitForText("Layout tree · pane-8")
  await term.press("Home")
  await term.press("ArrowRight") // root -> hidden Projects
  await term.press("ArrowDown") // Layout pane after compatible row flattening
  await term.press("ArrowRight") // A leaf has no child; selection stays put.
  await term.press("Enter")
  await term.waitForText("Edit pane-8")
  await term.press("Enter") // Insert before
  await term.waitForText("Choose pane job")
  await term.press("ArrowDown") // projects after activity
  await term.press("ArrowDown") // jobs
  await term.press("Enter")
  await term.waitForText("pane-9 jobs")
  await term.press("Enter")
  await term.press("End")
  await term.press("Enter") // Remove pane-8
  await term.waitForText("Remove pane-8?")
  await term.press("ArrowDown")
  await term.press("Enter")
  await expect.poll(() => term.find("Layout tree")).toBeNull()
  await term.type("after self removal")
  await term.waitForText("after self removal")
  await command(term, "/exit")
  expect(await term.waitForExit()).toBe(0)
})

test("a split Layout pane scrolls its tree and menus at the compact dock height", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 640 } })
  await term.waitForText("Message, !shell")
  await statusSessionId(term)
  await command(term, "/layout split left layout")
  await term.waitForText("Layout tree · pane-8")
  await term.press("End")
  await term.waitForText("pane-5 context")
  await term.press("Enter")
  await term.press("End")
  await term.waitForText("Remove")
  await term.press("Enter")
  await term.waitForText("Remove pane-5?")
  await term.press("ArrowDown")
  await term.press("Enter")
  await expect.poll(() => term.find("Remove pane-5?")).toBeNull()
  await term.press("End")
  await term.waitForText("pane-4 todos")
  await term.attach(testInfo, "compact-layout-editor")
  await command(term, "/layout close layout")
  await term.type("compact editor restored")
  await term.waitForText("compact editor restored")
})
