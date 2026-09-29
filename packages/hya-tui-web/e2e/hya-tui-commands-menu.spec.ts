// The separate `/` command pane (docs/tui.md "Command pane"): fuzzy filtering, key
// handling, merged local/server/skill sources, skill command turns, /compact,
// /rename, and /status, against the offline echo model or the fake model.
//
// Command-menu filtering runs off the command input's content-change event, which
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

const placeholder = "Message, !shell, or @file · / commands"

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

/** The command input is the row immediately above the pane's key hint. */
async function commandText(term: Tui): Promise<string | undefined> {
  return (await box(term, "Commands"))?.rows.at(-2)
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
  for (const width of [1100, 700]) {
    test(`Backspace closes an emptied command pane and preserves the message draft (${width}px)`, async ({ tui, backend }) => {
      const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
      await connected(term)
      await term.type("keep this draft")
      await expect.poll(() => composerText(term)).toBe("keep this draft")
      await term.press("Control+x")
      await term.type("/h")
      await term.waitForText("▸ /help")
      await term.press("Backspace")
      await expect.poll(() => commandText(term)).toBe("/")
      await term.press("Home")
      await term.press("Backspace")
      await expect.poll(() => commandText(term)).toBe("/")
      await term.press("End")
      await term.press("Backspace")
      await expect.poll(async () => (await box(term, "Commands")) === undefined).toBe(true)
      expect(await composerText(term)).toBe("keep this draft")
      await term.type(" continues")
      await expect.poll(() => composerText(term)).toBe("keep this draft continues")
      await term.press("Control+x")
      await term.type("/")
      await expect.poll(() => commandText(term)).toBe("/")
      await term.press("Escape")
      await expect.poll(async () => (await box(term, "Commands")) === undefined).toBe(true)
    })

    test(`Up/Down reach commands beyond the visible rows and wrap at the full list (${width}px)`, async ({ tui, backend }, testInfo) => {
      await writeSkill(backend.dir, "zz-navigation-last", "Last navigation choice", "Say hello.")
      const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
      await connected(term)
      await term.type("/")
      await term.waitForText("▸ /agent")
      await term.press("ArrowUp")
      await term.waitForText("▸ /zz-navigation-last")
      await term.press("ArrowDown")
      await term.waitForText("▸ /agent")

      const highlighted = async () => (await box(term, "Commands"))?.rows.find((row) => row.startsWith("▸ "))
      let selected = await highlighted()
      const visited = new Set([selected])
      for (let step = 0; step < 40 && !selected?.startsWith("▸ /layout "); step++) {
        await term.press("ArrowDown")
        await expect.poll(async () => {
          const next = await highlighted()
          return next !== undefined && next !== selected
        }).toBe(true)
        selected = await highlighted()
        expect(visited.has(selected), "navigation wrapped before reaching /layout").toBe(false)
        visited.add(selected)
        expect((await box(term, "Commands"))!.rows.filter((row) => row.includes("[local]") || row.includes("[command]") || row.includes("[skill]")).length).toBeLessThanOrEqual(8)
      }
      expect(selected).toMatch(/^▸ \/layout /)
      await term.attach(testInfo, "command-menu-scrolled")
      await term.press("Tab")
      await expect.poll(() => commandText(term)).toBe("/layout")
      await term.waitForText("▸ /layout assign")
      await term.press("Escape")
      await expect.poll(async () => (await box(term, "Commands")) === undefined).toBe(true)
    })
  }

  test("slash focuses a separate command pane without changing the message draft", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/")
    await term.waitForText("Commands")
    expect(await composerText(term)).toBe("")
    await term.type("help")
    expect(await composerText(term)).toBe("")
    await term.press("Escape")
    await expect.poll(async () => (await box(term, "Commands")) === undefined).toBe(true)

    await term.type("draft with /path")
    await expect.poll(() => composerText(term)).toBe("draft with /path")
    await term.press("Control+x")
    await term.type("/")
    await term.waitForText("Commands")
    expect(await composerText(term)).toBe("draft with /path")
  })

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

  test("the command pane shows second and third level layout choices while typing", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/layout ")
    await term.waitForText("▸ /layout assign")
    await term.waitForText("/layout split")
    await term.type("split ")
    await term.waitForText("▸ /layout split horizontal")
    await term.waitForText("/layout split vertical")
    await term.type("vertical ")
    await term.waitForText("/layout split vertical jobs")
    expect((await box(term, "Commands"))?.rows.some((row) => row.includes("/layout split vertical conversation"))).toBe(false)
  })

  test("Tab accepts nested choices and Enter runs the completed layout command", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/layout s")
    await term.waitForText("▸ /layout show")
    await term.press("ArrowDown")
    await term.waitForText("▸ /layout split")
    await term.press("Tab")
    await expect.poll(() => commandText(term)).toBe("/layout split")
    await term.waitForText("▸ /layout split horizontal")
    await term.press("ArrowDown")
    await term.waitForText("▸ /layout split vertical")
    await term.press("Tab")
    await expect.poll(() => commandText(term)).toBe("/layout split vertical")
    await term.type("jo")
    await term.waitForText("▸ /layout split vertical jobs")
    await term.press("Tab")
    await expect.poll(() => commandText(term)).toBe("/layout split vertical jobs")
    await term.press("Enter")
    await term.waitForText("jobs · pane-6")
  })

  test("the same nested menu shows API methods and paths", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/api ")
    await term.waitForText("/api GET")
    await term.type("GET /v1/hea")
    await term.waitForText("/api GET /v1/health")
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
    await expect.poll(() => commandText(term)).toBe("/models")
    expect((await box(term, "Commands"))?.rows.some((row) => row.includes("▸ /model"))).toBe(false)
    // Tab left a trailing space (keeps typing args), not a glued-on word.
    await term.type("x")
    await expect.poll(() => commandText(term)).toBe("/models x")
  })

  test("Esc closes the command pane and keeps its draft for reopening", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/hel")
    await term.waitForText("Commands")
    await term.press("Escape")
    await expect.poll(async () => (await box(term, "Commands")) === undefined).toBe(true)
    expect(await composerText(term)).toBe("")
    await term.type("/")
    await expect.poll(() => commandText(term)).toBe("/hel")
  })

  test("command history stays in the command pane and pasted slash text stays a message", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/status")
    await term.waitForText("▸ /status")
    await term.press("Enter")
    await term.waitForText("Status")
    await term.type("/")
    await term.press("Shift+ArrowUp")
    await expect.poll(() => commandText(term)).toBe("/status")
    await term.press("Escape")
    await expect.poll(async () => (await box(term, "Commands")) === undefined).toBe(true)
    await term.page.evaluate(() => window.hyaTerm.term.paste("/help"))
    await expect.poll(() => composerText(term)).toBe("/help")
    await term.press("Enter")
    await term.waitForText("Messages 2")
    expect(await term.find("Help · keys and commands")).toBeNull()
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
    await expect.poll(() => commandText(term)).toBe("/open")
    await term.type("1")
    await expect.poll(() => commandText(term)).toBe("/open 1")
  })
})

test.describe("skill commands", () => {
  test.use({ model: { steps: [textStep("hello from the greet skill")] } })

  test("a skill runs as a command turn; the transcript shows /name args, then the reply", async ({ tui, backend }) => {
    await writeSkill(backend.dir, "greet", "Say hello", "Say hello to $ARGUMENTS.")
    const term = await tui(hyaTui(backend))
    await connected(term)
    await term.type("/greet world")
    // The space after "greet" hides suggestions; the command input stays focused.
    await expect.poll(() => commandText(term)).toBe("/greet world")
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
    await term.waitForText("Agent       hya-main")
    await term.waitForText("Model       hya/offline")
    await term.waitForText("Mode        manual")
  })
})
