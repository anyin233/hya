// Key and command help (docs/tui.md "Key help"; Tier 1 G29): `?` on an
// empty input and `/help` open one filterable overlay listing every key
// binding by group and every command by source; Esc closes it; `?` inside
// text still types.

import type { Tui } from "./harness"
import { expect, hyaTui, test } from "./hya"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

const title = "Help · keys and commands"

test.describe("key help", () => {
  test("? on an empty input opens the overlay; it lists keys by group and filters; Esc closes", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("?")
    await term.waitForText(title)
    // Composer keys first, with the terminal-only note on Shift+Enter.
    await term.waitForText(/Enter \/ Keypad Enter\s+\[composer\]/)
    await term.attach(testInfo, "help-open")
    // Filter by a group name: the views keys.
    await term.type("views")
    await term.waitForText(/Ctrl\+B\s+\[views\]\s+Show or hide the sidebar/)
    await term.waitForText(/\?\s+\[views\]\s+Show every key and command/)
    expect(await term.find("[composer]")).toBeNull()
    await term.attach(testInfo, "help-filtered")
    await term.press("Escape")
    await expect.poll(() => term.find(title)).toBeNull()
    // The input kept the focus and is still empty: typing works.
    await term.type("hi")
    await term.waitForText("hi")
  })

  test("/help opens the same overlay with commands by source; Shift+Enter is marked terminal only", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/help")
    await term.waitForText(title)
    await term.type("shift+enter")
    await term.waitForText(/Shift\+Enter\s+\[composer\]\s+Insert a newline \(terminal only/)
    for (let i = 0; i < "shift+enter".length; i++) await term.press("Backspace")
    await term.type("/compact")
    await term.waitForText(/\/compact\s+\[local\]\s+Compact the session's context now/)
    for (let i = 0; i < "/compact".length; i++) await term.press("Backspace")
    // Server built-in commands from the backend catalog.
    await term.type("/review")
    await term.waitForText(/\/review.*\[server\]/)
    await term.press("Escape")
    await expect.poll(() => term.find(title)).toBeNull()
  })

  test("? inside text types a question mark instead of opening help", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("why?")
    await term.waitForText("why?")
    expect(await term.find(title)).toBeNull()
  })

  test("the overlay fits about 80 columns", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 640 } })
    await term.waitForText("Connected to hya")
    const { cols } = await term.size()
    await term.type("?")
    await term.waitForText(title)
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
    await term.attach(testInfo, "help-narrow")
  })
})
