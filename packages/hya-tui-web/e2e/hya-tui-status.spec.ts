// The working indicator, status bar, and live todo panel (docs/tui.md
// "Working indicator", "Status bar", "Todo panel"; Tier 1 E21-E23), driven
// against the scripted fake model. E24's `CompactionApplied` divider is
// covered by unit tests only (state/messages.test.ts,
// packages/hya-tui/test/messages.test.ts): the manual `/compact` command
// (`CompactSession`) injects a system-message marker, it does not emit a
// `CompactionApplied` event — only the engine's automatic mid-turn
// compaction strategies do, which this harness cannot trigger deterministically.

import type { Tui } from "./harness"
import { expect, hyaTui, initGitRepo, test, textStep, toolStep, toolsStep } from "./hya"

const spinner = /[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]/

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

async function at(term: Tui, needle: string) {
  const found = await term.find(needle)
  expect(found, `screen shows ${needle}`).not.toBeNull()
  return found!
}

/** Row/column of the first match of `pattern` on screen. */
async function match(term: Tui, pattern: RegExp): Promise<{ row: number; col: number }> {
  const lines = await term.lines()
  for (let row = 0; row < lines.length; row++) {
    const found = pattern.exec(lines[row]!)
    if (found) return { row, col: found.index }
  }
  throw new Error(`screen shows no ${pattern}`)
}

test.describe("working indicator", () => {
  test.use({
    model: {
      permission: "allow",
      steps: [toolStep("bash", { command: "sleep 2" }), textStep("Slept a bit, thanks for waiting around.", { chunkSize: 4, delayMs: 200 })],
    },
  })

  test("shows Running <tool>, then Writing…, with a spinner and elapsed time, and disappears when the turn ends", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "sleep then reply")

    // While the bash tool runs: `Running bash sleep 2`, a spinner, an mm:ss clock, the Esc hint.
    await term.waitForText(/Running bash sleep 2/, 20_000)
    const line = (await term.find("Running bash sleep 2"))!
    expect((await term.cell(line.row, 0))?.char).toMatch(spinner)
    await term.waitForText(/\d:\d\d · Running bash sleep 2 · Esc to interrupt/)
    await term.attach(testInfo, "working-running-tool")

    // Once the tool finishes and the answer streams: `Writing…`.
    await term.waitForText(/Writing…/, 20_000)
    await term.attach(testInfo, "working-writing")

    // The turn ends: the working line is gone.
    await term.waitForText("Slept a bit, thanks for waiting around.", 20_000)
    await term.waitForText(/^Ready/m)
    expect(await term.find("Esc to interrupt")).toBeNull()
  })
})

test.describe("streaming assistant header spinner", () => {
  test.use({ model: { steps: [textStep("delayed answer text", { chunkSize: 4, delayMs: 200 })] } })

  test("the assistant header spins while no body text has arrived yet", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "hello")
    // Before the first chunk lands, the assistant header's marker (column 0
    // of its row) is the spinner, not ●: distinguish it from the top header
    // line's `· build …`, which has no spinner glyph before the name.
    const running = new RegExp(`${spinner.source} build`)
    await term.waitForText(running, 20_000)
    const header = await match(term, running)
    expect((await term.cell(header.row, header.col))?.char).toMatch(spinner)
    await term.waitForText("delayed answer text", 20_000)
    await term.waitForText(/^Ready/m)
  })
})

const narrow = { width: 690, height: 640 }

test.describe("working indicator and status bar at 80 columns", () => {
  test.use({
    model: {
      permission: "allow",
      steps: [toolStep("bash", { command: "sleep 2" }), textStep("Slept a bit, thanks for waiting around.", { chunkSize: 4, delayMs: 200 })],
    },
  })

  test("both lines stay within the terminal width, sidebar hidden", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: narrow })
    await term.waitForText("Connected to hya")
    const { cols } = await term.size()
    expect(cols).toBeGreaterThanOrEqual(78)
    expect(cols).toBeLessThanOrEqual(84)
    await prompt(term, "sleep then reply")
    await term.waitForText(/Running bash sleep 2/, 20_000)
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
    await term.attach(testInfo, "narrow-working")
    await term.waitForText("Slept a bit, thanks for waiting around.", 20_000)
    await term.waitForText(/^Ready/m)
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
  })
})

test.describe("status bar", () => {
  test.use({ model: { steps: [textStep("status bar reply")] } })

  test("shows the permission mode, workspace directory, and git branch", async ({ tui, backend }) => {
    await initGitRepo(backend.dir, "main")
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "hi")
    await term.waitForText("status bar reply", 20_000)
    await term.waitForText(/^Ready/m)
    await term.waitForText("mode manual")
    await term.waitForText("⎇ main")
  })
})

test.describe("live todo panel", () => {
  test.use({
    model: {
      permission: "allow",
      steps: [
        toolStep("todo__update_content", { operations: [{ op: "add", content: "write tests" }] }),
        textStep("Added a todo."),
      ],
    },
  })

  test("the sidebar Todos box updates live from a real todo tool call; hiding the sidebar shows a compact count", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "track a todo")
    await term.waitForText("Added a todo.", 20_000)
    await term.waitForText(/^Ready/m)

    // Sidebar shown (default viewport, >=110 cols): the live item, pending glyph.
    await term.waitForText("○ write tests", 20_000)
    // Sidebar Context box: the merged transcript's message count (user + assistant).
    await term.waitForText("Messages 2")
    expect(await term.find("Todos 0/1")).toBeNull()

    // Ctrl+B hides the sidebar: the status bar shows the compact count instead.
    await term.press("Control+b")
    await term.waitForText("Todos 0/1", 20_000)
    expect(await term.find("write tests")).toBeNull()
  })
})
