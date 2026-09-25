// The composer (docs/tui.md "Composer"): multi-line input, input history,
// Esc interrupt, Ctrl+C / Ctrl+D / /exit quit, `!command` shell turns, and
// `@file` suggestions, against the offline echo model or the fake model.

import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hangStep, hyaTui, test } from "./hya"

const warning = "#e5c07b"
const accent = "#73c8e8"

/** Rows of the bordered composer box (the last box starting at column 0), inner text trimmed. */
async function composer(term: Tui): Promise<{ top: number; rows: string[]; title: string }> {
  const lines = await term.lines()
  let bottom = -1
  for (let row = lines.length - 1; row >= 0; row--) {
    if (lines[row]!.startsWith("└")) {
      bottom = row
      break
    }
  }
  let top = bottom - 1
  while (top >= 0 && !lines[top]!.startsWith("┌")) top--
  const rows = lines.slice(top + 1, bottom).map((line) => {
    const end = line.indexOf("│", 1)
    return line.slice(1, end < 0 ? undefined : end).trim()
  })
  const title = lines[top]!.slice(1, lines[top]!.indexOf("┐")).replace(/─/g, " ").trim()
  return { top, rows, title }
}

const placeholder = "Message, /command, !shell, or @file"

/** The composer's text; `""` while it shows the placeholder. */
async function composerText(term: Tui): Promise<string> {
  const text = (await composer(term)).rows.join("\n")
  return text === placeholder ? "" : text
}

async function connected(term: Tui): Promise<void> {
  await term.waitForText("Connected to hya")
}

test.describe("multi-line input", () => {
  test("Ctrl+J and Alt+Enter insert newlines, the box grows, and Enter sends one prompt", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    expect((await composer(term)).rows).toHaveLength(1)
    await term.type("line one")
    await term.press("Control+j")
    await term.type("line two")
    await term.press("Alt+Enter")
    await term.type("line three")
    await expect.poll(() => composerText(term)).toBe("line one\nline two\nline three")
    expect((await composer(term)).rows).toHaveLength(3)
    await term.attach(testInfo, "multiline-input")
    await term.press("Enter")
    await term.waitForText("● build · hya/offline", 20_000)
    // One user block with the three lines, in order, on consecutive rows.
    const first = (await term.find("┃ line one"))!
    const lines = await term.lines()
    expect(lines[first.row + 1]).toContain("┃ line two")
    expect(lines[first.row + 2]).toContain("┃ line three")
    expect(await composerText(term)).toBe("")
    expect((await composer(term)).rows).toHaveLength(1)
  })

  test("Shift+Enter sends in the browser: xterm.js reports it as a plain Enter (CR)", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("shift enter")
    await term.press("Shift+Enter")
    // Terminals with the kitty keyboard protocol report Shift+Enter and get a newline;
    // xterm.js 6 does not, so Ctrl+J / Alt+Enter are the newline keys in the WebUI.
    await term.waitForText("● build · hya/offline", 20_000)
    expect(await composerText(term)).toBe("")
  })

  test("the box stops growing at 8 rows and scrolls", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    for (let line = 1; line <= 11; line++) {
      await term.type(`row ${line}`)
      if (line < 11) await term.press("Control+j")
    }
    await expect.poll(async () => (await composer(term)).rows.at(-1)).toBe("row 11")
    const box = await composer(term)
    expect(box.rows).toHaveLength(8)
    expect(box.rows[0]).toBe("row 4")
  })

  test("a bracketed paste of several lines is inserted without sending", async ({ tui, backend, page }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("paste: ")
    // xterm.js wraps the text in bracketed-paste markers (the TUI enables mode 2004) and sends CRs.
    await page.evaluate(() => window.hyaTerm.term.paste("alpha\nbeta\ngamma"))
    await expect.poll(() => composerText(term)).toBe("paste: alpha\nbeta\ngamma")
    const text = await term.text()
    expect(text).not.toContain("● build")
    expect(text).toContain("No messages yet")
    expect((await composer(term)).rows).toHaveLength(3)
    await term.press("Enter")
    await term.waitForText("● build · hya/offline", 20_000)
    const first = (await term.find("┃ paste: alpha"))!
    expect((await term.lines())[first.row + 2]).toContain("┃ gamma")
  })

  test("cursor keys edit within the input: Home/End per line, Left, Backspace, Delete", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("abc")
    await term.press("Control+j")
    await term.type("xyz")
    await term.press("Home")
    await term.type(">")
    await term.press("End")
    await term.press("Backspace")
    await term.press("ArrowUp")
    await term.press("End")
    await term.press("ArrowLeft")
    await term.press("Delete")
    await expect.poll(() => composerText(term)).toBe("ab\n>xy")
  })
})

test.describe("input history", () => {
  test("Up and Down walk the submitted inputs and restore the draft", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    for (const text of ["first prompt", "second prompt"]) {
      await term.type(text)
      await term.press("Enter")
      await term.waitForText(`┃ ${text}`, 20_000)
    }
    await term.type("draft")
    await term.press("ArrowUp")
    await expect.poll(() => composerText(term)).toBe("second prompt")
    await term.press("ArrowUp")
    await expect.poll(() => composerText(term)).toBe("first prompt")
    await term.press("ArrowDown")
    await expect.poll(() => composerText(term)).toBe("second prompt")
    await term.press("ArrowDown")
    await expect.poll(() => composerText(term)).toBe("draft")
  })
})

test.describe("Esc and quitting", () => {
  test("Esc clears the input when no turn runs", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("throw this away")
    await expect.poll(() => composerText(term)).toBe("throw this away")
    await term.press("Escape")
    await expect.poll(() => composerText(term)).toBe("")
  })

  test("Ctrl+C once clears the input and shows the hint; twice quits with code 0", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("some text")
    await term.press("Control+c")
    await term.waitForText("Press Ctrl+C again to quit")
    await expect.poll(() => composerText(term)).toBe("")
    await term.attach(testInfo, "quit-hint")
    await term.press("Control+c")
    expect(await term.waitForExit()).toBe(0)
  })

  test("Ctrl+D on an empty input quits", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("x")
    await term.press("Control+d")
    // With text, Ctrl+D is a forward delete and does not quit.
    await term.press("Home")
    await term.press("Control+d")
    await expect.poll(() => composerText(term)).toBe("")
    await term.press("Control+d")
    expect(await term.waitForExit()).toBe(0)
  })

  test("/exit quits", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/exit")
    await term.press("Enter")
    expect(await term.waitForExit()).toBe(0)
  })
})

test.describe("Esc cancels a running turn", () => {
  test.use({ model: { steps: [hangStep()] } })

  test("cancels the turn and shows the cancelled state", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("hang please")
    await term.press("Enter")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await term.waitForText("Esc to interrupt")
    // Text typed while the turn runs stays: Esc cancels the turn, it does not clear.
    await term.type("next")
    await term.press("Escape")
    await term.waitForText("Cancelled · Ready", 20_000)
    await term.waitForText("! Cancelled")
    expect(await composerText(term)).toBe("next")
  })
})

test.describe("shell turns", () => {
  test("!command shows the shell indicator and runs as a shell turn", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("!echo hello")
    await expect.poll(async () => (await composer(term)).title).toBe("! shell")
    const box = await composer(term)
    expect((await term.cell(box.top, 0))?.fg).toBe(warning)
    await term.attach(testInfo, "shell-mode")
    await term.press("Enter")
    // The user block shows what was typed; the assistant shows the command under the bash call.
    await term.waitForText("┃ !echo hello", 20_000)
    await term.waitForText("$ echo hello")
    // The default permission policy asks before the shell tool runs, also for a user's shell turn;
    // the card waits for the answer. `/approve <id>` still answers it (the prompt's keyboard fallback).
    await term.waitForText(/perm_\w+/)
    await term.waitForText(/◌ bash\s+echo hello · awaiting approval/)
    expect((await term.cell((await term.find("◌ bash"))!.row, (await term.find("◌ bash"))!.col))?.fg).toBe(warning)
    const id = /perm_\w+/.exec(await term.text())![0]
    await term.type(`/approve ${id}`)
    await term.press("Enter")
    await term.waitForText(/✓ bash\s+echo hello/)
    await term.waitForText(/^Ready/m)
    expect(await term.text()).not.toContain("The following tool was executed by the user")
    const title = (await composer(term)).title
    expect(title).toBe("")
  })

  // A shell turn's bash card starts expanded: the command and its output show.
  // The ask is answered through the permission prompt (1 = Allow once).
  test("!echo hello shows the command output", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("!echo hello")
    await term.press("Enter")
    await term.waitForText("asked by build", 20_000)
    await term.press("1")
    await term.waitForText("$ echo hello", 20_000)
    await term.waitForText(/✓ bash\s+echo hello/, 20_000)
    await term.waitForText("│ hello")
  })
})

test.describe("@file references", () => {
  test("@ lists matching files from the work directory and Tab inserts the path", async ({ tui, backend }, testInfo) => {
    await writeFile(join(backend.dir, "notes-alpha.md"), "alpha\n")
    await writeFile(join(backend.dir, "other.txt"), "other\n")
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("read @notes")
    await term.waitForText("Files")
    await term.waitForText("▸ notes-alpha.md")
    expect(await term.text()).not.toContain("other.txt")
    const selected = (await term.find("▸ notes-alpha.md"))!
    expect((await term.cell(selected.row, selected.col))?.fg).toBe(accent)
    await term.attach(testInfo, "file-menu")
    await term.press("Tab")
    await expect.poll(() => composerText(term)).toBe("read @notes-alpha.md")
    expect(await term.text()).not.toContain("▸ notes-alpha.md")
    // Enter now sends the prompt with the reference as text.
    await term.type("please")
    await term.press("Enter")
    await term.waitForText("┃ read @notes-alpha.md please", 20_000)
  })

  test("at about 80 columns the list and a wrapped multi-line input fit the main column", async ({ tui, backend }, testInfo) => {
    await writeFile(join(backend.dir, "narrow-notes.md"), "n\n")
    const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 480 } })
    await connected(term)
    expect((await term.size()).cols).toBeLessThanOrEqual(84)
    await term.type("a first line that is long enough to wrap in an eighty column terminal window, twice over")
    await term.press("Control+j")
    await term.type("see @narrow")
    await term.waitForText("▸ narrow-notes.md")
    const box = await composer(term)
    expect(box.rows.length).toBeGreaterThanOrEqual(3)
    expect(box.rows.at(-1)).toBe("see @narrow")
    await term.attach(testInfo, "narrow-composer")
  })

  test("Esc closes the list, Up/Down select, and Enter inserts", async ({ tui, backend }) => {
    await writeFile(join(backend.dir, "file-a.md"), "a\n")
    await writeFile(join(backend.dir, "file-b.md"), "b\n")
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("@file-")
    await term.waitForText("▸ file-a.md")
    await term.press("Escape")
    await expect.poll(async () => (await term.text()).includes("file-b.md")).toBe(false)
    // Esc closed only the list; the text stays.
    expect(await composerText(term)).toBe("@file-")
    await term.press("Backspace")
    await term.type("-")
    await term.waitForText("▸ file-a.md")
    await term.press("ArrowDown")
    await term.waitForText("▸ file-b.md")
    await term.press("Enter")
    await expect.poll(() => composerText(term)).toBe("@file-b.md")
    expect(await term.text()).not.toContain("● build")
  })
})
