// Regression baseline of the tiled workspace (the `workspace` preset of
// ADR-0036, docs/tui-config.md): which regions the seven-pane tree draws at
// 80, 120, and 190 columns. Below 150 columns both sidebars are hidden and
// only the conversation, activity line, and editor remain; at 190 the left
// Projects pane and the right Sessions, Todos, and Context panes appear.

import { expect, hyaTui, test } from "./hya"
import type { Tui } from "./harness"

/** CSS width whose PTY conversion (tui-exp/driver.ts `viewportCells`) gives exactly `columns`. */
const widthFor = (columns: number) => Math.ceil(columns * 8.43 + 15)

async function regions(term: Tui) {
  const lines = await term.lines()
  const at = (pattern: RegExp) => lines.some((line) => pattern.test(line))
  return {
    projects: at(/─Projects─/),
    sessions: at(/─Sessions─/),
    todos: at(/^.*\sTodos\s*$|│\s*Todos\b|\bTodos\s{2,}/),
    context: lines.some((line) => /\bContext\b/.test(line) && !/Message, !shell/.test(line)),
    composer: at(/Message, !shell, or @file/),
  }
}

test.describe("workspace baseline", () => {
  test.use({ preset: "workspace" })

  for (const columns of [80, 120, 190]) {
    test(`regions at ${columns} columns`, async ({ tui, backend }, testInfo) => {
      const term = await tui(hyaTui(backend), { viewport: { width: widthFor(columns), height: 640 } })
      await term.waitForText("Message, !shell, or @file")
      expect((await term.size()).cols).toBe(columns)
      const wide = columns >= 150
      await expect.poll(async () => (await regions(term)).sessions, { timeout: 15_000 }).toBe(wide)
      const shown = await regions(term)
      expect(shown.composer).toBe(true)
      expect(shown.projects).toBe(wide)
      expect(shown.todos).toBe(wide)
      expect(shown.context).toBe(wide)
      await term.attach(testInfo, `workspace-${columns}`)
    })
  }
})
