// `/sessions` picker (C13) and session titles (G30, docs/tui.md "Pickers",
// "Session titles"): a `New session` row first, then the tree with a
// relative update time and agent/model, opening another session, renaming
// (F2) and deleting (Ctrl+D, with a confirmation line) a row, and a title
// set with `/rename` showing live in the header, the sidebar, and the
// picker (`SessionUpdated.title`, no extra refresh needed).

import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep } from "./hya"

const narrow = { width: 690, height: 640 }

test.use({ model: { steps: [textStep("First."), textStep("Second."), textStep("Third.")] } })

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  // Wait for the typed text to render before Enter, so a slow redraw right after a
  // previous action never drops characters and sends a shorter command instead.
  await term.waitForText(text)
  await term.press("Enter")
}

/** Create a session with `/new` and return its id, read from the header once it differs from `previousId`. */
async function newSession(term: Tui, previousId?: string): Promise<string> {
  await prompt(term, "/new")
  await expect.poll(async () => {
    const match = /hya · (hysec_\w+)/.exec(await term.text())
    return match?.[1] && match[1] !== previousId ? match[1] : undefined
  }, { timeout: 20_000 }).not.toBeUndefined()
  return /hya · (hysec_\w+)/.exec(await term.text())![1]!
}

/** The session the TUI opened on connect (a plain start creates one). */
async function connected(term: Tui): Promise<string> {
  await term.waitForText("Connected to hya")
  await term.waitForText(/hya · hysec_\w+/)
  return /hya · (hysec_\w+)/.exec(await term.text())![1]!
}

/**
 * `/rename` the open session, so it is kept when the TUI moves on (an empty
 * session this TUI created is deleted when it is left, docs/tui.md
 * "Sessions on start and exit").
 */
async function keep(term: Tui, title: string): Promise<void> {
  await prompt(term, `/rename ${title}`)
  await term.waitForText(new RegExp(`hya · ${title} ·`))
}

async function at(term: Tui, needle: string) {
  const found = await term.find(needle)
  expect(found, `screen shows ${needle}`).not.toBeNull()
  return found!
}

/** Esc, then wait for it to take effect before the next key — a lone Esc followed at once by another
 * key can be read as Alt+key by the terminal (see hya-tui-permission-modes.spec.ts `closePicker`). */
async function escape(term: Tui, goneText: string): Promise<void> {
  await term.press("Escape")
  await expect.poll(async () => (await term.text()).includes(goneText)).toBe(false)
}

test("lists a New session row first, then the tree with the open session marked; Enter opens another", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend))
  const first = await connected(term)
  await keep(term, "First session")
  const second = await newSession(term, first)

  await prompt(term, "/sessions")
  await term.waitForText("Sessions")
  await term.waitForText("New session")
  await term.waitForText(new RegExp(`▸ ● ${second}`))
  await term.waitForText("First session")
  await term.waitForText("F2 rename")
  await term.attach(testInfo, "sessions-picker")

  // Filtering narrows to the other session; Enter opens it.
  await term.type("First sess")
  await term.press("ArrowUp")
  await term.press("Enter")
  await term.waitForText(/hya · First session/)
})

test("an empty session this TUI created is deleted when it opens another; a used one is kept", async ({ tui, backend }) => {
  const term = await tui(hyaTui(backend))
  const empty = await connected(term)
  const kept = await newSession(term, empty)
  await prompt(term, "hello")
  await term.waitForText("First.", 20_000)
  await newSession(term, kept)
  const listed = async () => ((await (await fetch(`${backend.url}/v1/sessions`)).json()) as { sessions?: { id: string }[] }).sessions?.map((row) => row.id) ?? []
  await expect.poll(listed).not.toContain(empty)
  expect(await listed()).toContain(kept)
})

test("F2 renames the highlighted row; the title shows live in the header, the sidebar, and a reopened picker", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend))
  const session = await connected(term)

  await prompt(term, "/sessions")
  await term.waitForText("Sessions")
  // The open session is the picker's "current" row: already highlighted, no ArrowDown needed.
  await term.waitForText(new RegExp(`▸ ● ${session}`))
  await term.press("F2")
  await term.waitForText(new RegExp(`New title ${session}▏`))
  // Rename mode seeds the editable text with the row's current label (the raw id, no title set yet):
  // clear it before typing the new title.
  for (let index = 0; index < session.length; index++) await term.press("Backspace")
  await term.type("Fix the flaky test")
  await term.attach(testInfo, "sessions-rename")
  await term.press("Enter")
  await term.waitForText("Renamed to Fix the flaky test")

  // Header and sidebar (title over the raw id).
  await term.waitForText(/hya · Fix the flaky test ·/)
  await at(term, "Fix the flaky test")

  // A rename reopens the picker (so browsing continues) already showing the new title.
  await term.waitForText(/▸ ● Fix the flaky test/)
  await escape(term, "Filter ")
})

test("Ctrl+D shows a confirmation before deleting; Esc cancels, Enter deletes and opens another session", async ({ tui, backend }) => {
  const term = await tui(hyaTui(backend))
  const first = await connected(term)
  await keep(term, "First session")
  const second = await newSession(term, first)

  await prompt(term, "/sessions")
  await term.waitForText("Sessions")
  // The open session (second) is the picker's "current" row: already highlighted.
  await term.waitForText(new RegExp(`▸ ● ${second}`))
  await term.press("Control+d")
  await term.waitForText(/Delete .*Enter confirms · Esc cancels/)
  // Esc backs out to the list, still open; wait so the next Esc is not read as Alt+key.
  await escape(term, "Enter confirms")
  await term.waitForText("Sessions")
  await escape(term, "Filter ")

  await prompt(term, "/sessions")
  await term.waitForText(new RegExp(`▸ ● ${second}`))
  await term.press("Control+d")
  await term.waitForText(/Delete .*Enter confirms/)
  await term.press("Enter")
  await term.waitForText(`Deleted session ${second}`)
  // The open session was deleted: the other top-level session opens instead.
  await term.waitForText(/hya · First session/)

  await prompt(term, "/sessions")
  await term.waitForText("Sessions")
  expect(await term.text()).not.toContain(second)
})

test("fits about 80 columns", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { viewport: narrow })
  const { cols } = await term.size()
  expect(cols).toBeLessThanOrEqual(84)
  await term.waitForText("Connected to hya")
  await newSession(term)
  await prompt(term, "/sessions")
  await term.waitForText("Sessions")
  for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
  await term.attach(testInfo, "narrow-sessions-picker")
})
