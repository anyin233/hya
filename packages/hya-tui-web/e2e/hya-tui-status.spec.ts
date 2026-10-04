// Working indicators, sidebar metadata and Todos, and compaction dividers.
// Fake-model usage drives Context fields; a logging proxy verifies live todo
// updates arrive on the event stream without redundant reads.

import type { Tui } from "./harness"
import { createSession, expect, hyaTui, initGitRepo, showStatusView, statusSessionId, test, textStep, toolStep, toolsStep, wideViewport } from "./hya"
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
    await term.waitForText("Message, !shell, or @file · / commands")
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
    await term.waitForIdle()
    expect(await term.find("Esc to interrupt")).toBeNull()
  })
})

test.describe("streaming assistant header spinner", () => {
  // A full second before the first chunk: a 200 ms window was missed by the screen poll under a parallel suite's load.
  test.use({ model: { steps: [textStep("delayed answer text", { chunkSize: 8, delayMs: 1000 })] } })

  test("the assistant header spins while no body text has arrived yet", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "hello")
    // Before the first chunk lands, the assistant header's marker (column 0
    // of its row) is the spinner, not ●: distinguish it from the top header
    // line's `· hya-main …`, which has no spinner glyph before the name.
    const running = new RegExp(`${spinner.source} hya-main`)
    await term.waitForText(running, 20_000)
    const header = await match(term, running)
    expect((await term.cell(header.row, header.col))?.char).toMatch(spinner)
    await term.waitForText("delayed answer text", 20_000)
    await term.waitForIdle()
  })
})

const narrow = { width: 690, height: 640 }

test.describe("working indicator at 80 columns", () => {
  test.use({
    model: {
      permission: "allow",
      steps: [toolStep("bash", { command: "sleep 2" }), textStep("Slept a bit, thanks for waiting around.", { chunkSize: 4, delayMs: 200 })],
    },
  })

  test("the working line stays within the terminal width, sidebar hidden", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: narrow })
    await term.waitForText("Message, !shell, or @file · / commands")
    const { cols } = await term.size()
    expect(cols).toBeGreaterThanOrEqual(78)
    expect(cols).toBeLessThanOrEqual(84)
    await prompt(term, "sleep then reply")
    await term.waitForText(/Running bash sleep 2/, 20_000)
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
    await term.attach(testInfo, "narrow-working")
    await term.waitForText("Slept a bit, thanks for waiting around.", 20_000)
    await term.waitForIdle()
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
  })
})

test.describe("sidebar metadata", () => {
  test.use({ model: { steps: [textStep("status bar reply")] } })

  test("shows the permission mode, workspace directory, and git branch", async ({ tui, backend }) => {
    await initGitRepo(backend.dir, "main")
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "hi")
    await term.waitForText("status bar reply", 20_000)
    await term.waitForIdle()
    await term.waitForText(/Mode\s+manual/)
    await term.waitForText(/Branch\s+main/)
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

  test("the sidebar Todos box updates live from a real todo tool call; hiding the sidebar preserves the transcript", async ({ tui, backend }) => {
    // Through a logging proxy: the list must come from the `todoUpdated` frame, not a `GetSessionTodo` re-read.
    const proxy = await startProxy(backend.url)
    const term = await tui(hyaTui({ ...backend, url: proxy.url }), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    // The startup session is open (the Context box counts its messages): a prompt typed earlier
    // would create a second session, with its own seed read.
    await term.waitForText(/Messages\s+0/)
    await prompt(term, "track a todo")
    await term.waitForText("Added a todo.", 20_000)
    await term.waitForIdle()

    await term.waitForText("○ write te", 20_000)
    // One `GetSessionTodo`: the seed when the new session opened (empty then); the item came on the stream.
    expect(proxy.log.filter((entry) => entry.method === "GET" && /\/todo$/.test(entry.path))).toHaveLength(1)
    // Sidebar Context box: the merged transcript's message count (user + assistant).
    await term.waitForText("Messages 2")
    expect(await term.find("Todos 0/1")).toBeNull()

    // Hiding the sidebar removes its metadata without adding conversation headings.
    await term.press("Control+b")
    await expect.poll(() => term.find("─Todos")).toBeNull()
    expect(await term.find("○ write tests")).toBeNull()
  })
})

test.describe("sidebar context and tokens", () => {
  test.use({ model: { steps: [textStep("usage reply"), textStep("second usage reply")], contextLimit: 100_000 } })

  test("shows N% of the model's context limit and the session token total after a reply", async ({ tui, backend, fakeModel }, testInfo) => {
    fakeModel!.setUsage({ prompt: 42_000, completion: 300, reasoning: 0 })
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    // No usage reported yet: no percentage.
    expect(await term.find("0%")).toBeNull()
    await prompt(term, "hi")
    await term.waitForText("usage reply", 20_000)
    await term.waitForIdle()
    await term.waitForText(/Context\s+42%/)
    await term.waitForText("42.3k")
    const ctx = await at(term, "42%")
    expect((await term.cell(ctx.row, ctx.col))?.fg).toBe(colors.fg)
    // The sidebar's Context box shows the same usage with its window size.
    await term.waitForText("Context")
    await term.waitForText("Tokens")
    await term.attach(testInfo, "usage")

    await term.resize(690, 640)
    await expect.poll(() => term.find("Tokens")).toBeNull()
    await term.resize(1500, 640)
    // A fuller prompt crosses 80 %: Context uses the warning color.
    fakeModel!.setUsage({ prompt: 85_000, completion: 100, reasoning: 0 })
    await prompt(term, "again")
    await term.waitForText("second usage reply", 20_000)
    await term.waitForText(/Context\s+85%/)
    const warn = await at(term, "85%")
    expect((await term.cell(warn.row, warn.col))?.fg).toBe(colors.warning)
    await term.waitForText("127k")
  })
})

test.describe("compaction divider", () => {
  test.use({ model: { steps: [textStep("First answer before compaction."), textStep("Summary: the user said hi.")] } })

  test("/compact shows a manual divider with the folded message count before the summary", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "hi there")
    await term.waitForText("First answer before compaction.", 20_000)
    await term.waitForIdle()
    await prompt(term, "/compact")
    await term.waitForText(/── context compacted · \d+ messages? · manual · local summary ──/, 20_000)
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
    await first.waitForText("Message, !shell, or @file · / commands")
    await prompt(first, "hi there")
    await first.waitForText("First answer before compaction.", 20_000)
    await first.waitForIdle()
    await prompt(first, "/compact")
    await first.waitForText(/── context compacted · \d+ messages? · manual · local summary ──/, 20_000)
    await first.waitForText("Summary: the user said hi.", 20_000)
    await showStatusView(first)
    const session = await statusSessionId(first)
    await prompt(first, "/exit")
    await first.waitForExit()

    // A new TUI on the same session: the compaction happened before it opened.
    const second = await tui([...hyaTui(backend), "--session", session])
    await second.waitForText("Message, !shell, or @file · / commands")
    await dividerOnce(second)
    await second.attach(testInfo, "reopened")

    // Switch away and back.
    await createSession(second)
    expect(await second.find("context compacted")).toBeNull()
    await prompt(second, `/open ${session}`)
    await dividerOnce(second)
  })

  test("about 80 columns: the history divider fits one line", async ({ tui, backend }) => {
    const first = await tui(hyaTui(backend))
    await first.waitForText("Message, !shell, or @file · / commands")
    await prompt(first, "hi there")
    await first.waitForText("First answer before compaction.", 20_000)
    await first.waitForIdle()
    await prompt(first, "/compact")
    await first.waitForText("Summary: the user said hi.", 20_000)
    await showStatusView(first)
    const session = await statusSessionId(first)
    await prompt(first, "/exit")
    await first.waitForExit()

    const narrow = await tui([...hyaTui(backend), "--session", session], { viewport: { width: 690, height: 480 } })
    expect((await narrow.size()).cols).toBeLessThanOrEqual(84)
    await dividerOnce(narrow)
  })
})
