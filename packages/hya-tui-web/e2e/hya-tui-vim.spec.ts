// Vim mode (docs/tui.md "Vim mode"): `/vim` toggles it and saves `vim` in
// the preferences file; the cursor is a bar in insert mode and a block in normal;
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

/** Observe DECSCUSR through xterm's public parser API, without TUI internals. */
async function cursorStyle(term: Tui): Promise<number | null> {
  return term.inspect((terminal, state) => {
    if (!state.observedCursor) {
      state.observedCursor = { style: null }
      terminal.parser.registerCsiHandler({ intermediates: " ", final: "q" }, (params) => {
        state.observedCursor!.style = Number(params[0])
        return false
      })
    }
    return state.observedCursor.style
  })
}

/**
 * Esc, then wait for normal mode. A key sent in the same instant as Esc can
 * reach the TUI in one read with it (ESC b), which terminals mean as Alt+B;
 * a person never types that fast, a test driver does.
 */
async function normal(term: Tui): Promise<void> {
  await term.press("Escape")
  await expect.poll(() => cursorStyle(term)).toBe(2)
}

async function keys(term: Tui, sequence: string): Promise<void> {
  for (const key of sequence) await term.press(key)
}

test("/vim: insert and normal mode, motions, dd, undo, the cursor shape, and persistence", async ({ tui, backend }, testInfo) => {
  const prefs = join(dir, "tui.json")
  let term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
  await term.waitForText("Message, !shell, or @file · / commands")
  await cursorStyle(term)
  expect(await term.find("-- INSERT --")).toBeNull()

  await term.type("/vim")
  await term.press("Enter")
  await expect.poll(() => cursorStyle(term)).toBe(5)
  expect(JSON.parse(await readFile(prefs, "utf8"))).toEqual({ vim: true })

  await term.type("alpha beta gamma")
  await term.press("Escape")
  await expect.poll(() => cursorStyle(term)).toBe(2)
  // Esc in insert mode only switched modes: the text is still there.
  expect(await composerText(term)).toBe("alpha beta gamma")

  // Normal mode keys never type.
  await keys(term, "0wdw")
  await expect.poll(() => composerText(term)).toBe("alpha gamma")
  await term.press("u")
  await expect.poll(() => composerText(term)).toBe("alpha beta gamma")
  await term.press("Control+r")
  await expect.poll(() => composerText(term)).toBe("alpha gamma")
  // Delete a line and restore it through the editor's undo stack.
  await keys(term, "dd")
  await expect.poll(() => composerText(term)).toBe("Message, !shell, or @file · / commands")
  await term.press("u")
  await expect.poll(() => composerText(term)).toBe("alpha gamma")

  // A (append at the line end), type, Esc; x deletes; o opens a line.
  await keys(term, "A")
  await expect.poll(() => cursorStyle(term)).toBe(5)
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
  await expect.poll(() => composerText(term)).toBe("Message, !shell, or @file · / commands")
  expect(await term.find("next line")).not.toBeNull()
  await expect.poll(() => cursorStyle(term)).toBe(5)

  // A restarted TUI reads `vim: true` and starts in insert mode.
  term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs } })
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("/vim off")
  await term.press("Enter")
  await expect.poll(async () => JSON.parse(await readFile(prefs, "utf8")).vim).toBe(false)
  expect(JSON.parse(await readFile(prefs, "utf8"))).toEqual({ vim: false })
  // Off: letters type again.
  await term.type("hjkl")
  await expect.poll(() => composerText(term)).toBe("hjkl")
})

test("at about 80 columns Vim keys work with no status heading", async ({ tui, backend }) => {
  const prefs = join(dir, "tui.json")
  await writeFile(prefs, JSON.stringify({ vim: true }))
  const term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: prefs }, viewport: { width: 690, height: 640 } })
  await term.waitForText("Message, !shell, or @file · / commands")
  expect((await term.size()).cols).toBeLessThanOrEqual(84)
  await cursorStyle(term)
  await term.press("Escape")
  await expect.poll(() => cursorStyle(term)).toBe(2)
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
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("hang please")
    await term.press("Enter")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await term.waitForText("Esc to interrupt")
    await cursorStyle(term)
    await term.type("draft")
    await term.press("Escape")
    await expect.poll(() => cursorStyle(term)).toBe(2)
    // Still running: the first Esc only left insert mode.
    expect(await term.find("Cancelled")).toBeNull()
    await term.press("Escape")
    await term.waitForIdle(20_000)
    expect(await composerText(term)).toBe("draft")
    // No turn now: Esc in normal mode clears the input (the usual last meaning).
    await term.press("Escape")
    await expect.poll(() => composerText(term)).toBe("Message, !shell, or @file · / commands")
  })
})
