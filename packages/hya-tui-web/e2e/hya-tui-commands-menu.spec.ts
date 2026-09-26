// The `/` command menu (docs/tui.md "Command menu"): fuzzy filtering, key
// handling, merged local/server/skill sources, skill command turns, /compact,
// /rename, and /status, against the offline echo model or the fake model.
//
// Command-menu filtering runs off the composer's content-change event, which
// can lag one render behind fast programmatic typing; every spec waits for
// the specific highlighted `▸ /name` row before pressing Tab/Enter/ArrowDown,
// the same pattern the `@file` specs (hya-tui-composer.spec.ts) use.

import { mkdir, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep } from "./hya"

const accent = "#73c8e8"

async function connected(term: Tui): Promise<void> {
  await term.waitForText("Connected to hya")
}

/** Rows of the bordered box titled `title` (the topmost box whose border shows that title), trimmed. */
async function box(term: Tui, title: string): Promise<{ top: number; rows: string[] } | undefined> {
  const lines = await term.lines()
  const top = lines.findIndex((line) => line.startsWith("┌") && line.includes(title))
  if (top < 0) return undefined
  let bottom = top + 1
  while (bottom < lines.length && !lines[bottom]!.startsWith("└")) bottom++
  const rows = lines.slice(top + 1, bottom).map((line) => {
    const end = line.indexOf("│", 1)
    return line.slice(1, end < 0 ? undefined : end).trim()
  })
  return { top, rows }
}

const placeholder = "Message, /command, !shell, or @file"

/** The composer's text (trailing spaces are trimmed by the row reader, same as the `@file` specs). */
async function composerText(term: Tui): Promise<string> {
  const lines = await term.lines()
  const bottom = lines.findLastIndex((line) => line.startsWith("└"))
  let top = bottom - 1
  while (top >= 0 && !lines[top]!.startsWith("┌")) top--
  const text = lines.slice(top + 1, bottom).map((line) => {
    const end = line.indexOf("│", 1)
    return line.slice(1, end < 0 ? undefined : end).trim()
  }).join("\n")
  return text === placeholder ? "" : text
}

async function writeSkill(dir: string, name: string, description: string, body: string): Promise<void> {
  const skillDir = join(dir, ".hya/skills", name)
  await mkdir(skillDir, { recursive: true })
  await writeFile(join(skillDir, "SKILL.md"), `---\nname: ${name}\ndescription: ${description}\n---\n${body}\n`)
}

/**
 * Type `/new`, then Enter: its argument hint (`[agent] [model]`) is
 * bracketed/optional (commands/menu.ts `requiresArgument`), so the menu's
 * Enter runs it immediately — replacing the composer text and submitting in
 * the same call (Composer.tsx `acceptCommandEntry`). The composer showing
 * "/new" is a one-frame transient on the way to submitting, not a stable
 * state to poll for (it can already be gone by the first check under load),
 * so wait for the actual outcome instead.
 */
async function createSessionViaMenu(term: Tui): Promise<void> {
  await term.type("/new")
  await term.waitForText("▸ /new")
  await term.press("Enter")
  await term.waitForText(/^Created/m)
}

test.describe("command menu", () => {
  test("typing / opens the menu; typing filters it by name, sources tagged", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/")
    await term.waitForText("Commands")
    await term.waitForText("▸ /agent")
    await term.waitForText("[local]")
    await term.attach(testInfo, "command-menu-open")

    await term.type("rev")
    await term.waitForText("▸ /review")
    const menu = (await box(term, "Commands"))!
    expect(menu.rows.some((row) => row.includes("/review"))).toBe(true)
    expect(menu.rows.some((row) => row.includes("/agent"))).toBe(false)
  })

  test("Up/Down move the highlight, Tab completes the name and keeps typing args", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/mod")
    await term.waitForText("▸ /model")
    const first = (await term.find("▸ /model"))!
    expect((await term.cell(first.row, first.col))?.fg).toBe(accent)
    await term.press("ArrowDown")
    await term.waitForText("▸ /models")
    await term.press("Tab")
    await expect.poll(() => composerText(term)).toBe("/models")
    expect((await box(term, "Commands"))).toBeUndefined()
    // Tab left a trailing space (keeps typing args), not a glued-on word.
    await term.type("x")
    await expect.poll(() => composerText(term)).toBe("/models x")
  })

  test("Esc closes the menu and keeps the typed text", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/hel")
    await term.waitForText("Commands")
    await term.press("Escape")
    await expect.poll(async () => (await box(term, "Commands")) === undefined).toBe(true)
    expect(await composerText(term)).toBe("/hel")
  })

  test("Enter on a command with no arguments runs it; Enter on one with an argument hint completes and waits", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/help")
    await term.waitForText("▸ /help")
    await term.press("Enter")
    // /help opens the key and command help overlay; Esc closes it.
    await term.waitForText("Help · keys and commands")
    await term.press("Escape")
    await expect.poll(() => term.find("Help · keys and commands")).toBeNull()

    await term.type("/open")
    await term.waitForText("▸ /open")
    await term.press("Enter")
    await expect.poll(() => composerText(term)).toBe("/open")
    await term.type("1")
    await expect.poll(() => composerText(term)).toBe("/open 1")
  })
})

test.describe("skill commands", () => {
  test.use({ model: { steps: [textStep("hello from the greet skill")] } })

  test("a skill runs as a command turn; the transcript shows /name args, then the reply", async ({ tui, backend }) => {
    await writeSkill(backend.dir, "greet", "Say hello", "Say hello to $ARGUMENTS.")
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/greet world")
    // The space after "greet" closes the command menu; wait for the editor
    // (and this test's own sync()) to settle before Enter, so Enter submits
    // the prompt rather than racing a still-open menu (see the header note).
    await expect.poll(() => composerText(term)).toBe("/greet world")
    await term.press("Enter")
    await term.waitForText("┃ /greet world", 20_000)
    await term.waitForText("hello from the greet skill")
  })

  test("the menu tags a skill as a distinct source from a server command", async ({ tui, backend }) => {
    await writeSkill(backend.dir, "greet", "Say hello", "Say hello to $ARGUMENTS.")
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/gre")
    await term.waitForText("▸ /greet")
    const menu = (await box(term, "Commands"))!
    const row = menu.rows.find((line) => line.includes("/greet"))!
    expect(row).toContain("[skill]")
  })
})

test.describe("/compact", () => {
  test.use({ model: { steps: [textStep("hi there")] } })

  test("shows a compacting status, then the outcome", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("hi")
    await term.press("Enter")
    await term.waitForText(/^Ready/m, 20_000)
    await term.type("/compact")
    await term.waitForText("▸ /compact")
    await term.press("Enter")
    await term.attach(testInfo, "compacting")
    // A manual compaction is always `local_summarizer`, shown in words (docs/protocol/README.md "Compaction").
    await term.waitForText("Compacted · local summary", 20_000)
  })
})

test.describe("/rename", () => {
  test("updates the header and sidebar title", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await createSessionViaMenu(term)
    await term.type("/rename Bug fix session")
    await term.press("Enter")
    await term.waitForText("Renamed to Bug fix session")
    await term.waitForText("hya · Bug fix session ·")
    await term.press("Control+b")
    await term.waitForText("Bug fix session")
  })
})

test.describe("/status", () => {
  test("shows server, version, directory, session, agent, model, and permission mode", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await createSessionViaMenu(term)
    await term.type("/status")
    await term.waitForText("▸ /status")
    await term.press("Enter")
    await term.waitForText("Status")
    await term.waitForText("Server      http")
    await term.waitForText("Directory   ")
    await term.waitForText("Session     hysec_")
    await term.waitForText("Agent       build")
    await term.waitForText("Model       hya/offline")
    await term.waitForText("Mode        manual")
  })
})
