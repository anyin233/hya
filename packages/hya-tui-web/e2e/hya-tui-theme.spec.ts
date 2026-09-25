// Themes (docs/tui.md "Themes"): `/theme` opens a picker of the built-in
// themes; moving the highlight previews a theme on the whole screen, Esc
// restores the theme in effect before, Enter keeps it and saves it to the
// preferences file (`HYA_TUI_CONFIG` points it at a temp path here), and a
// restarted TUI starts in the saved theme.

import { mkdtemp, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep } from "./hya"

// Palettes of packages/hya-tui/src/theme.ts.
const hya = { bg: "#11151b", fg: "#e8edf3", muted: "#9caab9", accent: "#73c8e8" }
const light = { bg: "#f7f9fb", fg: "#1f2933", muted: "#5b6b7b", accent: "#0b6f94", panel: "#e6ecf2", keyword: "#8839c9" }

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

/** Background of the main column (a cell of the empty transcript area) and the header's text color. */
async function screenColors(term: Tui): Promise<{ bg: string; header: string }> {
  const lines = await term.lines()
  // The first blank row under the header and status bar belongs to the transcript.
  const row = lines.findIndex((line, index) => index > 1 && line.slice(0, 20).trim() === "")
  const bg = (await term.cell(row, 1))?.bg ?? "none"
  const header = await term.find("hya")
  const fg = header ? (await term.cell(header.row, header.col))?.fg ?? "none" : "none"
  return { bg, header: fg }
}

let prefsDir: string
test.beforeEach(async () => { prefsDir = await mkdtemp(join(tmpdir(), "hya-tui-theme-")) })
test.afterEach(async () => { await rm(prefsDir, { recursive: true, force: true }) })

test.describe("/theme", () => {
  test("previews the highlighted theme, Esc restores, Enter saves; a restarted TUI starts in the saved theme", async ({ tui, backend }, testInfo) => {
    const prefs = join(prefsDir, "prefs", "tui.json")
    const env = { HYA_TUI_CONFIG: prefs }
    let term = await tui(hyaTui(backend), { env })
    await term.waitForText("Connected to hya")
    expect(await screenColors(term)).toEqual({ bg: hya.bg, header: hya.accent })

    await prompt(term, "/theme")
    await term.waitForText("Theme")
    await term.waitForText(/▸ ● hya\s+\[dark\]/)
    await term.waitForText(/ {3}Light\s+\[light\]/)
    await term.waitForText(/High contrast\s+\[dark\]/)
    await term.waitForText(/Ember\s+\[dark\]/)

    // Down highlights Light: the whole screen previews it at once.
    await term.press("ArrowDown")
    await expect.poll(async () => (await screenColors(term)).bg).toBe(light.bg)
    await term.attach(testInfo, "light-preview")
    // Esc restores the default theme and saves nothing.
    await term.press("Escape")
    await expect.poll(() => term.find("Theme ·")).toBeNull()
    await expect.poll(async () => (await screenColors(term)).bg).toBe(hya.bg)
    await expect(readFile(prefs, "utf8")).rejects.toThrow()

    // Enter keeps the highlighted theme and writes the preferences file.
    await prompt(term, "/theme")
    await term.waitForText(/▸ ● hya/)
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText("Theme → Light")
    expect(await screenColors(term)).toEqual({ bg: light.bg, header: light.accent })
    expect(JSON.parse(await readFile(prefs, "utf8"))).toEqual({ theme: "light" })

    // The status line follows the theme too.
    const status = await term.find("Theme → Light")
    expect((await term.cell(status!.row, status!.col))?.fg).toBe(light.muted)

    // A new TUI reads the file and starts in the light theme; the picker marks it.
    term = await tui(hyaTui(backend), { env })
    await term.waitForText("Connected to hya")
    await expect.poll(async () => (await screenColors(term)).bg).toBe(light.bg)
    await prompt(term, "/theme")
    await term.waitForText(/▸ ● Light\s+\[light\]/)
    await term.press("Escape")
    await expect.poll(() => term.find("● Light")).toBeNull()
    expect((await screenColors(term)).bg).toBe(light.bg)
  })

  test("at about 80 columns the light theme covers the screen and the picker fits", async ({ tui, backend }) => {
    const env = { HYA_TUI_CONFIG: join(prefsDir, "tui.json") }
    const term = await tui(hyaTui(backend), { env, viewport: { width: 690, height: 640 } })
    await term.waitForText("Connected to hya")
    expect((await term.size()).cols).toBeLessThanOrEqual(84)
    await prompt(term, "/theme")
    await term.waitForText(/Light\s+\[light\]/)
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText("Theme → Light")
    const { cols } = await term.size()
    // The right edge of the main column is themed too (no stale dark cells).
    const lines = await term.lines()
    const row = lines.findIndex((line, index) => index > 1 && line.trim() === "")
    expect((await term.cell(row, cols - 1))?.bg).toBe(light.bg)
    expect((await term.cell(row, 0))?.bg).toBe(light.bg)
    // The status line and the footer instruction sit on the themed background.
    const footer = await term.find("Enter a prompt")
    const status = await term.find("Theme → Light")
    expect(await term.cell(footer!.row, footer!.col + 1)).toMatchObject({ fg: light.muted, bg: light.bg })
    expect(await term.cell(status!.row, status!.col + 1)).toMatchObject({ fg: light.muted, bg: light.bg })
  })
})

test.describe("/theme over a transcript", () => {
  test.use({ model: { steps: [textStep("Here is **bold** code:\n\n```ts\nconst answer = 42\n```\n\nDone.")] } })

  test("switching repaints existing messages: the user block, Markdown text, and highlighted code", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { env: { HYA_TUI_CONFIG: join(prefsDir, "tui.json") } })
    await term.waitForText("Connected to hya")
    await prompt(term, "show me code")
    await term.waitForText("Done.", 20_000)
    await prompt(term, "/theme")
    await term.waitForText(/Light\s+\[light\]/)
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText("Theme → Light")
    const user = (await term.find("show me code"))!
    expect(await term.cell(user.row, user.col)).toMatchObject({ fg: light.fg, bg: light.panel })
    const done = (await term.find("Done."))!
    expect((await term.cell(done.row, done.col))?.fg).toBe(light.fg)
    const code = (await term.find("const answer"))!
    await expect.poll(async () => (await term.cell(code.row, code.col))?.fg).toBe(light.keyword)
    expect((await term.cell(code.row, code.col))?.bg).toBe(light.panel)
    await term.attach(testInfo, "light-transcript")
  })
})
