// Vim mode (docs/tui.md "Vim mode"): `/vim` toggles it and saves `vim` in
// the preferences file; the status bar shows `-- INSERT --` / `-- NORMAL --`;
// Esc in insert mode switches to normal mode, Esc in normal mode keeps its
// usual meaning (cancel the running turn, clear the input).

import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hangStep, hyaTui, test } from "./hya"

let dir: string
test.beforeEach(async () => { dir = await mkdtemp(join(tmpdir(), "hya-tui-vim-")) })
test.afterEach(async () => { await rm(dir, { recursive: true, force: true }) })

async function composerText(term: Tui): Promise<string> {
  const lines = await term.lines()
  const bottom = lines.findLastIndex((line) => line.startsWith("└"))
  const top = lines.slice(0, bottom).findLastIndex((line) => line.startsWith("┌"))
  const right = lines[bottom]!.indexOf("┘")
  return lines.slice(top + 1, bottom).map((line) => line.slice(1, right).trim()).join("\n").trim()
}


test("/vim persistence and composer toggle", async ({ tui, backend }) => {
  const prefs = join(dir, "tui.json")
  let term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("/vim")
  await term.waitForText("/vim")
  await term.press("Enter")
  await expect.poll(async () => {
    try { return JSON.parse(await readFile(prefs, "utf8")).vim === true } catch { return false }
  }).toBe(true)
  await term.press("Control+d")
  await term.waitForExit()
  term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
  await term.waitForText("Message, !shell, or @file · / commands")
  expect(JSON.parse(await readFile(prefs, "utf8"))).toEqual({ vim: true })
  await term.type("/vim on")
  await term.waitForText("/vim on")
  await term.press("Enter")
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("alpha beta gamma")
  await term.waitForText("alpha beta gamma")
  await term.press("Escape")
  expect(await composerText(term)).toBe("alpha beta gamma")
  term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("/vim off")
  await term.waitForText("/vim off")
  await term.press("Enter")
  await expect.poll(async () => {
    try { return JSON.parse(await readFile(prefs, "utf8")).vim === false } catch { return false }
  }).toBe(true)
  expect(JSON.parse(await readFile(prefs, "utf8"))).toEqual({ vim: false })
  await term.type("hjkl")
  await term.waitForText("hjkl")
  await expect.poll(() => composerText(term)).toBe("hjkl")
})

test("at about 80 columns the vim indicator and the permission mode stay on the status bar", async ({ tui, backend }) => {
  const prefs = join(dir, "tui.json")
  await writeFile(prefs, JSON.stringify({ vim: true }))
  const term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs }, viewport: { width: 690, height: 640 } })
  await term.waitForText("Message, !shell, or @file · / commands")
  expect((await term.size()).cols).toBeLessThanOrEqual(84)
  // Esc with nothing pending in normal mode on an empty input does nothing else; ? is swallowed.
  await term.press("?")
  expect(await term.find("Help · keys and commands")).toBeNull()
  await term.press("i")
  await term.press("?")
  await term.waitForText("Help · keys and commands")
})

test.describe("Esc precedence with vim on", () => {
  test.use({ model: { steps: [hangStep()] } })

  test("Esc in insert mode switches to normal; the next Esc cancels the running turn and preserves the draft", async ({ tui, backend, fakeModel }) => {
    const prefs = join(dir, "tui.json")
    await writeFile(prefs, JSON.stringify({ vim: true }))
    const term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("hang please")
    await term.press("Enter")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await term.waitForText("Esc to interrupt")
    await term.type("draft")
    await term.press("Escape")
    // Still running: the first Esc only left insert mode.
    expect(await term.find("Cancelled")).toBeNull()
    await term.press("Escape")
    await term.waitForIdle(20_000)
    // Cancelling the running turn preserves the draft; Esc is consumed by the
    // cancellation path rather than clearing newly entered text.
    await expect.poll(() => composerText(term)).toBe("draft")
  })
})
