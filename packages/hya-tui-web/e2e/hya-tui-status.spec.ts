// The working indicator, status bar, live todo panel, and compaction
// divider (docs/tui.md "Working indicator", "Status bar", "Todo panel",
// "Notices"; Tier 1 E21-E24), driven against the scripted fake model: the
// status bar's `ctx N%` and token total from reported usage and a model
// context limit, `todoUpdated` frames (no todo re-reads, checked through a
// logging proxy), and the `/compact` divider, live and after reopening.

import type { Tui } from "./harness"
import { expect, hyaTui, initGitRepo, test, textStep, toolStep, toolsStep } from "./hya"
import { startProxy } from "./proxy"

const spinner = /[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]/
const colors = { fg: "#e8edf3", muted: "#9caab9", warning: "#e5c07b", error: "#f07878" }

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
    // Through a logging proxy: the list must come from the `todoUpdated` frame, not a `GetSessionTodo` re-read.
    const proxy = await startProxy(backend.url)
    const term = await tui(hyaTui({ ...backend, url: proxy.url }))
    await term.waitForText("Connected to hya")
    await prompt(term, "track a todo")
    await term.waitForText("Added a todo.", 20_000)
    await term.waitForText(/^Ready/m)

    // Sidebar shown (default viewport, >=110 cols): the live item, pending glyph.
    await term.waitForText("○ write tests", 20_000)
    // One `GetSessionTodo`: the seed when the new session opened (empty then); the item came on the stream.
    expect(proxy.log.filter((entry) => entry.method === "GET" && /\/todo$/.test(entry.path))).toHaveLength(1)
    // Sidebar Context box: the merged transcript's message count (user + assistant).
    await term.waitForText("Messages 2")
    expect(await term.find("Todos 0/1")).toBeNull()

    // Ctrl+B hides the sidebar: the status bar shows the compact count instead.
    await term.press("Control+b")
    await term.waitForText("Todos 0/1", 20_000)
    expect(await term.find("write tests")).toBeNull()
  })
})

test.describe("status bar context and tokens", () => {
  test.use({ model: { steps: [textStep("usage reply"), textStep("second usage reply")], contextLimit: 100_000 } })

  test("shows ctx N% of the model's context limit and the session token total after a reply", async ({ tui, backend, fakeModel }, testInfo) => {
    fakeModel!.setUsage({ prompt: 42_000, completion: 300, reasoning: 0 })
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    // Unknown before any reply: hidden, not `ctx 0%`.
    expect(await term.find("ctx ")).toBeNull()
    await prompt(term, "hi")
    await term.waitForText("usage reply", 20_000)
    await term.waitForText(/^Ready/m)
    await term.waitForText(/mode manual · ctx 42% · 42\.3k tok/)
    const ctx = await at(term, "ctx 42%")
    expect((await term.cell(ctx.row, ctx.col))?.fg).toBe(colors.muted)
    // The sidebar's Context box shows the same, with the window size.
    await term.waitForText("Context  42% · 42k/100k")
    await term.waitForText("Tokens   42.3k")
    await term.attach(testInfo, "usage")

    // A fuller prompt crosses 80 %: the segment turns the warning color.
    fakeModel!.setUsage({ prompt: 85_000, completion: 100, reasoning: 0 })
    await prompt(term, "again")
    await term.waitForText("second usage reply", 20_000)
    await term.waitForText("ctx 85%")
    const warn = await at(term, "ctx 85%")
    expect((await term.cell(warn.row, warn.col))?.fg).toBe(colors.warning)
    await term.waitForText("127k tok")
  })
})

test.describe("compaction divider", () => {
  test.use({ model: { steps: [textStep("First answer before compaction."), textStep("Summary: the user said hi.")] } })

  test("/compact shows a manual divider with the folded message count before the summary", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "hi there")
    await term.waitForText("First answer before compaction.", 20_000)
    await term.waitForText(/^Ready/m)
    await prompt(term, "/compact")
    await term.waitForText(/── context compacted · \d+ messages? · manual ──/, 20_000)
    const divider = await match(term, /── context compacted/)
    expect(divider.row).toBeGreaterThan((await at(term, "First answer before compaction.")).row)
    expect((await term.cell(divider.row, divider.col))?.fg).toBe(colors.muted)
    await term.attach(testInfo, "compacted")
  })
})

test.describe("compaction divider in history", () => {
  test.use({ model: { steps: [textStep("First answer before compaction."), textStep("Summary: the user said hi.")] } })

  /** The divider sits between the folded answer and the summary, exactly once. */
  async function dividerOnce(term: Tui): Promise<void> {
    await term.waitForText(/── context compacted ──/, 20_000)
    await term.waitForText("First answer before compaction.")
    const text = await term.text()
    expect(text.match(/context compacted/g)).toHaveLength(1)
    const divider = await match(term, /── context compacted ──/)
    expect(divider.row).toBeGreaterThan((await at(term, "First answer before compaction.")).row)
    expect(divider.row).toBeLessThan((await at(term, "Summary: the user said hi.")).row)
    expect((await term.cell(divider.row, divider.col))?.fg).toBe(colors.muted)
  }

  test("a session opened later (a new TUI, or switching back) shows its past compaction at the same place, once", async ({ tui, backend }, testInfo) => {
    const first = await tui(hyaTui(backend))
    await first.waitForText("Connected to hya")
    await prompt(first, "hi there")
    await first.waitForText("First answer before compaction.", 20_000)
    await first.waitForText(/^Ready/m)
    await prompt(first, "/compact")
    await first.waitForText(/── context compacted · \d+ messages? · manual ──/, 20_000)
    await first.waitForText("Summary: the user said hi.", 20_000)
    const session = /hya · (hysec_\w+)/.exec(await first.text())![1]!
    await prompt(first, "/exit")
    await first.waitForExit()

    // A new TUI on the same session: the compaction happened before it opened.
    const second = await tui([...hyaTui(backend), "--session", session])
    await second.waitForText("Connected to hya")
    await dividerOnce(second)
    await second.attach(testInfo, "reopened")

    // Switch away and back.
    await prompt(second, "/new")
    await second.waitForText(/Created hysec_/, 20_000)
    expect(await second.find("context compacted")).toBeNull()
    await prompt(second, `/open ${session}`)
    await dividerOnce(second)
  })

  test("about 80 columns: the history divider fits one line", async ({ tui, backend }) => {
    const first = await tui(hyaTui(backend))
    await first.waitForText("Connected to hya")
    await prompt(first, "hi there")
    await first.waitForText("First answer before compaction.", 20_000)
    await first.waitForText(/^Ready/m)
    await prompt(first, "/compact")
    await first.waitForText("Summary: the user said hi.", 20_000)
    const session = /hya · (hysec_\w+)/.exec(await first.text())![1]!
    await prompt(first, "/exit")
    await first.waitForExit()

    const narrow = await tui([...hyaTui(backend), "--session", session], { viewport: { width: 690, height: 480 } })
    expect((await narrow.size()).cols).toBeLessThanOrEqual(84)
    await dividerOnce(narrow)
  })
})
