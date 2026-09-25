// Tool call cards and subagent visibility (docs/tui.md "Tool calls" and
// "Subagents"): state icons, per-tool summaries, collapse/expand (Ctrl+G,
// /tools, a click on the header), diff colors, failures, the running
// spinner, and a `task` card whose child session opens read-only, driven by
// the fake model.

import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hangStep, hyaTui, test, textStep, toolStep, toolsStep } from "./hya"

const colors = {
  fg: "#e8edf3", muted: "#9caab9", accent: "#73c8e8", error: "#f07878", warning: "#e5c07b",
  done: "#a5d6a7", add: "#a5d6a7", remove: "#f07878",
}

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

/** Row and column of the first match of `pattern` on the screen. */
async function match(term: Tui, pattern: RegExp): Promise<{ row: number; col: number }> {
  const lines = await term.lines()
  for (let row = 0; row < lines.length; row++) {
    const found = pattern.exec(lines[row]!)
    if (found) return { row, col: found.index }
  }
  throw new Error(`screen shows no ${pattern}`)
}

/** Click the terminal cell at `row`, `col`. */
async function click(term: Tui, row: number, col: number): Promise<void> {
  const screen = (await term.page.locator(".xterm-screen").boundingBox())!
  const size = await term.size()
  await term.page.mouse.click(screen.x + ((col + 0.5) / size.cols) * screen.width, screen.y + ((row + 0.5) / size.rows) * screen.height)
}

test.describe("read card", () => {
  test.use({ model: { steps: [toolStep("read", { path: "notes.txt", offset: 1, limit: 2 }), textStep("Read the notes.")] } })

  test("a finished read shows ✓, the path, the line range, and the duration; a click expands it", async ({ tui, backend }, testInfo) => {
    await writeFile(join(backend.dir, "notes.txt"), "alpha line\nbeta line\ngamma line\n")
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "read my notes")
    await term.waitForText("Read the notes.", 20_000)
    await term.waitForText(/✓ read\s+notes\.txt · lines 1-2 of 3\s+\d+ms/)
    const icon = await at(term, "✓ read")
    // A blank row separates the card from the reply text after it.
    expect((await at(term, "Read the notes.")).row).toBe(icon.row + 2)
    expect((await term.cell(icon.row, icon.col))?.fg).toBe(colors.done)
    expect((await term.cell(icon.row, icon.col + 2))?.fg).toBe(colors.fg)
    expect((await term.cell(icon.row, icon.col + "✓ read  ".length))?.fg).toBe(colors.muted)
    // Collapsed by default: the file content is not shown.
    expect(await term.find("alpha line")).toBeNull()
    await term.attach(testInfo, "read-collapsed")

    await click(term, icon.row, icon.col + 4)
    await term.waitForText(/1\s+alpha line/)
    await term.waitForText(/2\s+beta line/)
    expect(await term.find("gamma line")).toBeNull()
    // The click leaves the input focused.
    await prompt(term, "/tools off")
    await term.waitForText("Tool calls collapsed · Ctrl+G toggles")
    expect(await term.find("alpha line")).toBeNull()
  })
})

test.describe("bash card", () => {
  test.use({
    model: {
      permission: "allow",
      steps: [toolStep("bash", { command: "printf 'first\\nsecond\\n'" }), textStep("Ran it.")],
    },
  })

  test("the command is the summary; Ctrl+G and /tools expand and collapse the output", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "run printf")
    await term.waitForText("Ran it.", 20_000)
    await term.waitForText(/✓ bash\s+printf 'first\\nsecond\\n'\s+\d+ms/)
    expect(await term.find("$ printf")).toBeNull()

    await term.press("Control+g")
    await term.waitForText("$ printf 'first\\nsecond\\n'")
    await term.waitForText("│ first")
    const output = await at(term, "│ second")
    expect((await term.cell(output.row, output.col + 2))?.fg).toBe(colors.muted)
    await term.waitForText("Tool calls expanded · Ctrl+G toggles")
    await term.attach(testInfo, "bash-expanded")

    await term.press("Control+g")
    await expect.poll(() => term.find("$ printf")).toBeNull()
    await prompt(term, "/tools on")
    await term.waitForText("$ printf")
    await prompt(term, "/tools")
    await expect.poll(() => term.find("$ printf")).toBeNull()
  })
})

test.describe("edit and write cards", () => {
  test.use({
    model: {
      permission: "allow",
      steps: [
        toolsStep([
          { name: "edit", arguments: { path: "poem.txt", edits: [{ op: "replace_text", oldText: "two", newText: "TWO" }] } },
          { name: "write", arguments: { path: "fresh.txt", content: "brand\nnew\n" } },
        ]),
        textStep("Edited."),
      ],
    },
  })

  test("edits and writes show the path and a colored diff", async ({ tui, backend }, testInfo) => {
    await writeFile(join(backend.dir, "poem.txt"), "one\ntwo\nthree\n")
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "edit the poem")
    await term.waitForText("Edited.", 20_000)
    await term.waitForText(/✓ edit\s+poem\.txt · \+1 -1/)
    await term.waitForText(/✓ write\s+fresh\.txt · 2 lines/)
    await prompt(term, "/tools on")
    await term.waitForText("- two")
    const removed = await at(term, "- two")
    expect((await term.cell(removed.row, removed.col))?.fg).toBe(colors.remove)
    const added = await at(term, "+ TWO")
    expect((await term.cell(added.row, added.col))?.fg).toBe(colors.add)
    const context = await at(term, "  three")
    expect((await term.cell(context.row, context.col + 2))?.fg).toBe(colors.muted)
    const hunk = await at(term, "@@ -1,3 +1,3 @@")
    expect((await term.cell(hunk.row, hunk.col))?.fg).not.toBe(colors.muted)
    const written = await at(term, "+ brand")
    expect((await term.cell(written.row, written.col))?.fg).toBe(colors.add)
    await term.attach(testInfo, "diff")
  })
})

test.describe("failed tool", () => {
  test.use({ model: { steps: [toolStep("read", { path: "missing.txt" }), textStep("It is missing.")] } })

  test("a failed call shows ✗ and its error message in the error color", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "read the missing file")
    await term.waitForText("It is missing.", 20_000)
    await term.waitForText(/✗ read\s+missing\.txt/)
    const icon = await at(term, "✗ read")
    expect((await term.cell(icon.row, icon.col))?.fg).toBe(colors.error)
    await term.waitForText("File not found")
    const message = await at(term, "File not found")
    expect((await term.cell(message.row, message.col))?.fg).toBe(colors.error)
    // The error line shows even while the card is collapsed.
    expect(message.row).toBe(icon.row + 1)
  })
})

test.describe("running tool", () => {
  test.use({ model: { permission: "allow", steps: [toolStep("bash", { command: "sleep 2" }), textStep("Slept.")] } })

  test("a running call animates a spinner, then shows ✓ and its duration", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "sleep a bit")
    await term.waitForText(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] bash\s+sleep 2/, 20_000)
    const row = (await term.find("bash  sleep 2"))!
    const first = await term.cell(row.row, row.col - 2)
    expect(first?.char).toMatch(spinner)
    expect(first?.fg).toBe(colors.accent)
    await term.attach(testInfo, "running")
    // The frame advances while the tool runs.
    await expect.poll(async () => (await term.cell(row.row, row.col - 2))?.char, { timeout: 3_000 }).not.toBe(first?.char)
    await term.waitForText("Slept.", 20_000)
    await term.waitForText(/✓ bash\s+sleep 2\s+2\.\ds/)
  })
})

test.describe("subagents", () => {
  test.use({ model: { steps: [] } })

  test("a task card shows the child's status and activity; the child opens read-only and Esc returns", async ({ tui, backend, fakeModel }, testInfo) => {
    await writeFile(join(backend.dir, "notes.txt"), "alpha\n")
    // The parent's and the child's requests are told apart by their system prompts.
    fakeModel!.route("NEVER call `report`", [
      toolStep("task", { description: "survey the repo", prompt: "list the files", subagent_type: "general" }),
      textStep("Spawned a helper."),
    ])
    fakeModel!.route("Finish your task with `report`", [toolStep("read", { path: "notes.txt" }), hangStep(20_000)])
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "delegate the survey")
    await term.waitForText("Spawned a helper.", 20_000)
    await term.waitForText(/task\s+general · survey the repo/)
    // The child is still working (its model request hangs): running, with its latest tool call.
    await term.waitForText(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] running/, 15_000)
    await term.waitForText("↳ read notes.txt", 15_000)
    const status = await match(term, /[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] running/)
    expect((await term.cell(status.row, status.col))?.fg).toBe(colors.accent)
    expect((await term.cell(status.row, status.col + 2))?.fg).toBe(colors.accent)
    // The sidebar nests the child session under its parent.
    await term.waitForText(/↳ 2\. general · running/)
    await term.attach(testInfo, "task-running")

    // A click on the card opens the child read-only.
    const card = await at(term, "general · survey the repo")
    await click(term, card.row, card.col + 2)
    await term.waitForText("Viewing subagent general · Esc returns")
    await term.waitForText("Read-only subagent view · Esc returns to the parent")
    await term.waitForText("┃ list the files")
    await term.waitForText(/✓ read\s+notes\.txt/)
    await term.attach(testInfo, "child-view")
    await prompt(term, "can I type here")
    await term.waitForText("Read-only: this is a subagent's session · Esc returns to the parent")
    expect(await term.find("┃ can I type here")).toBeNull()

    // Esc returns to the parent; the typed text stays, and a second Esc clears it.
    await term.press("Escape")
    await term.waitForText("Back to the parent session")
    await term.waitForText("Spawned a helper.")
    expect(await term.find("Viewing subagent")).toBeNull()
    await term.waitForText("│ can I type here")
    await term.press("Escape")
    await expect.poll(() => term.find("can I type here")).toBeNull()

    // The child finishes its turn: the card says idle.
    fakeModel!.release()
    await term.waitForText(/✓ idle/, 15_000)

    // /open with the child's id opens it too.
    const child = /hysec_\w+/.exec((await term.text()).split("\n").find((line) => line.includes("/open hysec_")) ?? "")?.[0]
    expect(child).toBeTruthy()
    await prompt(term, `/open ${child}`)
    await term.waitForText("Viewing subagent general · Esc returns")
    await term.press("Escape")
    await term.waitForText("Spawned a helper.")
  })
})

test.describe("narrow terminal", () => {
  test.use({ model: { permission: "allow", steps: [toolStep("bash", { command: "echo narrow-output && ls" }), textStep("Done.")] } })

  test("cards fit about 80 columns", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 480 } })
    expect((await term.size()).cols).toBeLessThanOrEqual(84)
    await term.waitForText("Connected to hya")
    await prompt(term, "run it")
    await term.waitForText("Done.", 20_000)
    await prompt(term, "/tools on")
    await term.waitForText(/│ narrow-output/)
    const header = await at(term, "✓ bash")
    expect((await term.lines())[header.row]).toMatch(/✓ bash\s+echo narrow-output && ls\s+\d+ms/)
    await term.attach(testInfo, "narrow")
  })
})
