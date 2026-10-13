import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, expect, hyaTui, statusSessionId, test, textStep } from "./hya"

const reply = Array.from({ length: 100 }, (_, i) => `line ${i}`).join("\n\n") + "\n\nDOCK END"
async function gap(term: Tui): Promise<number> {
  const end = (await term.find("DOCK END"))!
  const input = (await term.find("Message, !shell, or @file · / commands"))!
  return input.row - 1 - end.row // editor's top border minus final transcript line
}

test.describe("compact conversation dock", () => {
  test.use({ model: { steps: [textStep(reply)] } })
  for (const viewport of [{ width: 1100, height: 640 }, { width: 690, height: 640 }, { width: 1100, height: 960 }]) {
    test(`viewer meets idle input at ${viewport.width}x${viewport.height}; multiline input expands`, async ({ tui, backend }, testInfo) => {
      const term = await tui(hyaTui(backend), { viewport })
      await term.waitForText("Message, !shell, or @file · / commands")
      await statusSessionId(term)
      await term.type("long reply please")
      await term.press("Enter")
      await term.waitForText("DOCK END", 20_000)
      await term.waitForIdle()
      await expect.poll(() => gap(term)).toBeLessThanOrEqual(2)
      await term.attach(testInfo, "compact-idle")
      await term.paste("FIRST DRAFT ROW\nSECOND DRAFT ROW\nTHIRD DRAFT ROW")
      for (const row of ["FIRST DRAFT ROW", "SECOND DRAFT ROW", "THIRD DRAFT ROW"]) await term.waitForText(row)
      const first = (await term.find("FIRST DRAFT ROW"))!
      const third = (await term.find("THIRD DRAFT ROW"))!
      expect(third.row - first.row).toBe(2)
      await term.attach(testInfo, "compact-multiline")
    })
  }

  test("saved proportional default dock upgrades without layout reset", async ({ tui, backend }, testInfo) => {
    await writeFile(join(backendConfigDir(backend), "tui.json"), JSON.stringify({ paneLayout: { version: 3, active: "pane-1", root: {
      type: "split", axis: "horizontal", weight: 0.8,
      first: { type: "pane", id: "pane-2", kind: "conversation" },
      second: { type: "split", axis: "horizontal", weight: 0.2,
        first: { type: "pane", id: "pane-3", kind: "activity" }, second: { type: "pane", id: "pane-1", kind: "composer" },
      },
    } } }))
    const term = await tui(hyaTui(backend), { viewport: { width: 1100, height: 960 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    await statusSessionId(term)
    await term.type("long reply please")
    await term.press("Enter")
    await term.waitForText("DOCK END", 20_000)
    await term.waitForIdle()
    await expect.poll(() => gap(term)).toBeLessThanOrEqual(2)
    await term.attach(testInfo, "compact-migrated")
  })
})

test.describe("active dock", () => {
  test.use({ model: { steps: [textStep("streaming slowly ".repeat(15) + "ACTIVE DONE", { chunkSize: 3, delayMs: 30 })] } })
  test("activity occupies exactly the row immediately above input", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await statusSessionId(term)
    await term.type("stream a reply")
    await term.press("Enter")
    await term.waitForText("Esc to interrupt", 20_000)
    await expect.poll(async () => {
      const activity = await term.find("Esc to interrupt")
      const input = await term.find("Message, !shell, or @file · / commands")
      return activity && input ? input.row - 1 - activity.row : -1
    }).toBe(1)
    await term.attach(testInfo, "compact-active")
    await term.waitForText("ACTIVE DONE", 20_000)
    await term.waitForIdle()
  })
})
