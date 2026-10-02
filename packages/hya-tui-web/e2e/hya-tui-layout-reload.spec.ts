import { readFile, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { backendConfigDir, expect, hyaTui, statusSessionId, test } from "./hya"

const leaf = (id: number, kind: string) => ({ type: "pane", id: `pane-${id}`, kind })
const initial = { version: 3, active: "pane-1", root: {
  type: "split", axis: "horizontal", weight: 0.8, sizing: "content-second",
  first: leaf(2, "conversation"), second: leaf(1, "composer"),
} }

for (const width of [1100, 690]) {
  test(`reload externally edited layout preserves draft and file (${width}px)`, async ({ tui, backend }, testInfo) => {
    const path = join(backendConfigDir(backend), "tui.json")
    await writeFile(path, JSON.stringify({ paneLayout: initial, theme: "hya" }))
    const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    const session = await statusSessionId(term)
    await term.type("draft kept across reload")
    await term.waitForText("draft kept across reload")
    const before = (await term.find("draft kept across reload"))!
    const edited = { ...initial, root: { type: "split", axis: "vertical", weight: 0.5,
      first: initial.root, second: leaf(3, "status"),
    } }
    const text = JSON.stringify({ paneLayout: edited, theme: "light", unknown: "preserve me" }, null, 2)
    await writeFile(path, text)
    await term.press("Control+x")
    await term.type("/layout reload")
    await term.press("Enter")
    await term.waitForText("status · pane-3")
    await expect.poll(() => term.find("draft kept across reload")).not.toBeNull()
    const draft = (await term.find("draft kept across reload"))!
    expect(draft.col).toBe(before.col)
    const status = (await term.find("status · pane-3"))!
    expect(status.col).toBeGreaterThan(draft.col)
    // Only layout was reloaded; light theme in the file stays unapplied.
    expect((await term.cell(draft.row - 1, draft.col))?.fg).toBe("#73c8e8")
    expect(await readFile(path, "utf8")).toBe(text)
    await term.attach(testInfo, "reloaded-with-draft")

    // Invalid replacement must retain the same running tree and draft.
    const invalid = JSON.stringify({ paneLayout: { ...edited, root: leaf(1, "composer") } })
    await writeFile(path, invalid)
    await term.press("Control+x")
    await term.type("/layout reload")
    await term.press("Enter")
    await expect.poll(() => term.find("Commands")).toBeNull()
    await term.waitForText("draft kept across reload")
    await term.waitForText("status · pane-3")
    expect(await readFile(path, "utf8")).toBe(invalid)
    await term.waitForText(`Session     ${session}`)
    await term.attach(testInfo, "invalid-reload-retains-layout")
  })
}
