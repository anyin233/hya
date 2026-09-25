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

/** The status bar (the row under the header). */
async function statusBar(term: Tui): Promise<string> {
  return (await term.lines())[1] ?? ""
}

/**
 * Esc, then wait for normal mode. A key sent in the same instant as Esc can
 * reach the TUI in one read with it (ESC b), which terminals mean as Alt+B;
 * a person never types that fast, a test driver does.
 */
async function normal(term: Tui): Promise<void> {
  await term.press("Escape")
  await expect.poll(() => statusBar(term)).toMatch(/^-- NORMAL --/)
}

async function keys(term: Tui, sequence: string): Promise<void> {
  for (const key of sequence) await term.press(key)
}

test("/vim: insert and normal mode, motions, dd, undo, the status bar indicator, and persistence", async ({ tui, backend }, testInfo) => {
  const prefs = join(dir, "tui.json")
  let term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
  await term.waitForText("Connected to hya")
  expect(await statusBar(term)).not.toContain("INSERT")

  await term.type("/vim")
  await term.press("Enter")
  await term.waitForText("Vim mode on · Esc for normal mode, i to insert")
  await expect.poll(() => statusBar(term)).toMatch(/^-- INSERT -- · mode manual/)
  expect(JSON.parse(await readFile(prefs, "utf8"))).toEqual({ vim: true })
  // The indicator is drawn in the muted color in insert mode, the accent color in normal mode.
  expect((await term.cell(1, 3))?.fg).toBe("#9caab9")

  await term.type("alpha beta gamma")
  await term.press("Escape")
  await expect.poll(() => statusBar(term)).toMatch(/^-- NORMAL --/)
  expect((await term.cell(1, 3))?.fg).toBe("#73c8e8")
  // Esc in insert mode only switched modes: the text is still there.
  expect(await composerText(term)).toBe("alpha beta gamma")

  // Normal mode keys never type.
  await keys(term, "0wdw")
  await expect.poll(() => composerText(term)).toBe("alpha gamma")
  await term.press("u")
  await expect.poll(() => composerText(term)).toBe("alpha beta gamma")
  await term.press("Control+r")
  await expect.poll(() => composerText(term)).toBe("alpha gamma")
  // A half-typed command shows next to the mode.
  await keys(term, "2d")
  await expect.poll(() => statusBar(term)).toMatch(/^-- NORMAL -- 2d · /)
  await term.press("Escape")
  await expect.poll(() => statusBar(term)).toMatch(/^-- NORMAL -- · /)
  await keys(term, "dd")
  await expect.poll(() => composerText(term)).toBe("Message, /command, !shell, or @file")
  await term.press("u")
  await expect.poll(() => composerText(term)).toBe("alpha gamma")

  // A (append at the line end), type, Esc; x deletes; o opens a line.
  await keys(term, "A")
  await expect.poll(() => statusBar(term)).toMatch(/^-- INSERT --/)
  await term.type(" delta")
  await normal(term)
  await keys(term, "bx")
  await expect.poll(() => composerText(term)).toBe("alpha gamma elta")
  await keys(term, "o")
  await term.type("next line")
  await normal(term)
  await expect.poll(() => composerText(term)).toBe("alpha gamma elta\nnext line")
  await keys(term, "kdd")
  await expect.poll(() => composerText(term)).toBe("next line")
  await term.attach(testInfo, "normal-mode")

  // Enter in normal mode sends; the next input starts in insert mode.
  await term.press("Enter")
  await expect.poll(() => composerText(term)).toBe("Message, /command, !shell, or @file")
  expect(await term.find("next line")).not.toBeNull()
  await expect.poll(() => statusBar(term)).toMatch(/^-- INSERT --/)

  // A restarted TUI reads `vim: true` and starts in insert mode.
  term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
  await term.waitForText("Connected to hya")
  await expect.poll(() => statusBar(term)).toMatch(/^-- INSERT --/)
  await term.type("/vim off")
  await term.press("Enter")
  await term.waitForText("Vim mode off")
  await expect.poll(() => statusBar(term)).toMatch(/^mode manual/)
  expect(JSON.parse(await readFile(prefs, "utf8"))).toEqual({ vim: false })
  // Off: letters type again.
  await term.type("hjkl")
  await expect.poll(() => composerText(term)).toBe("hjkl")
})

test("at about 80 columns the vim indicator and the permission mode stay on the status bar", async ({ tui, backend }) => {
  const prefs = join(dir, "tui.json")
  await writeFile(prefs, JSON.stringify({ vim: true }))
  const term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs }, viewport: { width: 690, height: 640 } })
  await term.waitForText("Connected to hya")
  expect((await term.size()).cols).toBeLessThanOrEqual(84)
  await expect.poll(() => statusBar(term)).toMatch(/^-- INSERT -- · mode manual/)
  await term.press("Escape")
  await expect.poll(() => statusBar(term)).toMatch(/^-- NORMAL -- · mode manual/)
  // Esc with nothing pending in normal mode on an empty input does nothing else; ? is swallowed.
  await term.press("?")
  expect(await term.find("Help · keys and commands")).toBeNull()
  await term.press("i")
  await term.press("?")
  await term.waitForText("Help · keys and commands")
  await term.type("vim")
  await term.waitForText("h j k l")
})

test.describe("Esc precedence with vim on", () => {
  test.use({ model: { steps: [hangStep()] } })

  test("Esc in insert mode switches to normal; the next Esc cancels the running turn; with no turn it clears the input", async ({ tui, backend, fakeModel }) => {
    const prefs = join(dir, "tui.json")
    await writeFile(prefs, JSON.stringify({ vim: true }))
    const term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
    await term.waitForText("Connected to hya")
    await term.type("hang please")
    await term.press("Enter")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await term.waitForText("Esc to interrupt")
    await term.type("draft")
    await term.press("Escape")
    await expect.poll(() => statusBar(term)).toMatch(/^-- NORMAL --/)
    // Still running: the first Esc only left insert mode.
    expect(await term.find("Cancelled")).toBeNull()
    await term.press("Escape")
    await term.waitForText("Cancelled · Ready", 20_000)
    expect(await composerText(term)).toBe("draft")
    // No turn now: Esc in normal mode clears the input (the usual last meaning).
    await term.press("Escape")
    await expect.poll(() => composerText(term)).toBe("Message, /command, !shell, or @file")
  })
})
