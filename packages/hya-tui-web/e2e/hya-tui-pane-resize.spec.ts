import { readFile, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, expect, hyaTui, statusSessionId, test } from "./hya"

async function moveToCell(term: Tui, col: number, row: number) {
  const box = await term.screenBox(), { cols, rows } = await term.size()
  await term.mouse.move(box.x + (col + .5) / cols * box.width, box.y + (row + .5) / rows * box.height)
}

for (const width of [1100, 690]) {
  test(`divider hover shows its axis without changing focus or layout (${width}px)`, async ({ tui, backend }) => {
    const leaf = (id: number, kind: string) => ({ type: "pane", id: `pane-${id}`, kind })
    const weight = (node: unknown) => ({ node, size: { mode: "weight", value: 1 } })
    const paneLayout = { version: 4, active: "pane-1", root: { type: "split", id: "group-1", direction: "row", children: [
      weight(leaf(8, "jobs")), weight({ type: "split", id: "group-2", direction: "column", children: [
        weight(leaf(9, "workflows")), weight(leaf(6, "conversation")), { node: leaf(1, "composer"), size: { mode: "content" } },
      ] }),
    ] } }
    const path = join(backendConfigDir(backend), "tui.json")
    const saved = JSON.stringify({ paneLayout })
    await writeFile(path, saved)
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell")
    await statusSessionId(term)
    await term.type("draft survives hover")
    await term.waitForText("draft survives hover")
    await term.inspect((terminal, state) => {
      state.osc22 = []
      terminal.parser.registerOscHandler(22, (payload: string) => { state.osc22!.push(payload); return false })
    })
    const draft = (await term.find("draft survives hover"))!
    const divider = draft.col - 2
    const { rows, cols } = await term.size()
    await moveToCell(term, divider, 2)
    await term.waitForText("↔")
    expect(await term.find("↔")).toEqual({ row: Math.floor(rows / 2), col: divider })
    expect((await term.cell(Math.floor(rows / 2), divider))?.fg).toBe("#535353")
    await expect.poll(() => term.inspect((_terminal, state) => state.osc22?.some((value) => value.includes("ew-resize")))).toBe(true)
    expect((await term.cell(draft.row - 1, draft.col + 4))?.fg).toBe("#73c8e8")
    expect(await readFile(path, "utf8")).toBe(saved)
    await moveToCell(term, divider - 4, 2)
    await expect.poll(() => term.find("↔")).toBeNull()
    await expect.poll(() => term.inspect((_terminal, state) => state.osc22?.at(-1))).toBe("default")

    // A horizontal divider moves vertically, within this nested column only.
    await moveToCell(term, draft.col + 4, draft.row - 1)
    await term.waitForText("↕")
    expect(await term.find("↕")).toEqual({ row: draft.row - 1, col: divider + Math.floor((cols - divider) / 2) })
    expect((await term.cell(draft.row - 1, divider + 1))?.char).toBe("─")
    await expect.poll(() => term.inspect((_terminal, state) => state.osc22?.some((value) => value.includes("ns-resize")))).toBe(true)
    expect(await readFile(path, "utf8")).toBe(saved)

    // Opening an overlay clears the guide even without another pointer event.
    await term.press("Control+x")
    await term.type("/")
    await term.waitForText("Commands")
    await expect.poll(() => term.find("↕")).toBeNull()
    await term.press("Escape")
    await expect.poll(() => term.find("Commands")).toBeNull()
    await moveToCell(term, divider - 4, 2)
    await expect.poll(() => term.find("↕")).toBeNull()
    await term.type(" continues")
    await term.waitForText("draft survives hover continues")
    await moveToCell(term, divider, 2)
    await term.waitForText("↔")
    await term.mouse.down()
    await moveToCell(term, divider + 3, 2)
    await expect.poll(async () => (await term.find("↔"))?.col).toBe(divider + 3)
    await term.focus(false)
    await expect.poll(() => term.find("↔")).toBeNull()
    await term.mouse.up()
    await expect.poll(async () => (await term.find("draft survives hover continues"))?.col).toBe(draft.col)
    expect(await readFile(path, "utf8")).toBe(saved)
    await term.focus(true)
  })
}
