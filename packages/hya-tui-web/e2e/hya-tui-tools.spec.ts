// Tool call cards and subagent visibility (docs/tui.md "Tool calls" and
// "Subagents"): state icons, per-tool summaries, collapse/expand (Ctrl+G,
// /tools, a click on the header), diff colors, failures, the running
// spinner, and a `task` card whose child session opens read-only, driven by
// the fake model.

import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hangStep, hyaTui, outlinedToolCard, test, textStep, toolStep, toolsStep, wideViewport } from "./hya"

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
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "read my notes")
    await term.waitForText("Read the notes.", 20_000)
    await term.waitForText(outlinedToolCard("✓", "read", '"path":"notes.txt","offset":1,"limit":2'))
    const icon = await at(term, "✓")
    // A blank row separates the card from the reply text after it.
    expect((await at(term, "Read the notes.")).row).toBeGreaterThan(icon.row + 2)
    expect((await term.cell(icon.row, icon.col))?.fg).toBe(colors.done)
    const title = await at(term, "read")
    expect((await term.cell(title.row, title.col))?.fg).toBe(colors.fg)
    // Collapsed by default: the file content is not shown.
    expect(await term.find("alpha line")).toBeNull()
    await term.attach(testInfo, "read-collapsed")

    await click(term, icon.row, icon.col + 4)
    await term.waitForText(/1\s+alpha line/)
    await term.waitForText(/2\s+beta line/)
    expect(await term.find("gamma line")).toBeNull()
    // The click leaves the input focused.
    await prompt(term, "/tools off")
    await expect.poll(() => term.find("alpha line")).toBeNull()
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
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "run printf")
    await term.waitForText("Ran it.", 20_000)
    await term.waitForText(outlinedToolCard("✓", "bash", '"command":"printf \'first\\\\nsecond\\\\n\'"'))
    expect(await term.find("$ printf")).toBeNull()

    await term.press("Control+g")
    await term.waitForText("first")
    await term.waitForText("│ first")
    const output = await at(term, "│ second")
    expect((await term.cell(output.row, output.col + 2))?.fg).toBe(colors.muted)
    await term.attach(testInfo, "bash-expanded")

    await term.press("Control+g")
    await expect.poll(() => term.find("│ first")).toBeNull()
    await prompt(term, "/tools on")
    await term.waitForText("first")
    await prompt(term, "/tools")
    await expect.poll(() => term.find("│ first")).toBeNull()
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
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "edit the poem")
    await term.waitForText("Edited.", 20_000)
    await term.waitForText(outlinedToolCard("✓", "edit", '"path":"poem.txt"'))
    await term.waitForText(outlinedToolCard("✓", "write", '"path":"fresh.txt"'))
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
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "read the missing file")
    await term.waitForText("It is missing.", 20_000)
    await term.waitForText(outlinedToolCard("✗", "read", '"path":"missing.txt"'))
    const icon = await at(term, "✗")
    expect((await term.cell(icon.row, icon.col))?.fg).toBe(colors.error)
    expect(await term.find("File not found")).toBeNull()
    await click(term, icon.row, icon.col + 2)
    await term.waitForText("File not found")
    const message = await at(term, "File not found")
    expect((await term.cell(message.row, message.col))?.fg).toBe(colors.error)
  })
})

test.describe("running tool", () => {
  test.use({ model: { permission: "allow", steps: [toolStep("bash", { command: "sleep 2" }), textStep("Slept.")] } })

  test("a running call animates a spinner, then shows ✓ and its duration", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "sleep a bit")
    await term.waitForText("sleep 2", 20_000)
    const running = await match(term, /[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]/)
    const spinner = await term.cell(running.row, running.col)
    expect(spinner?.char).toMatch(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]/)
    expect(spinner?.fg).toBe(colors.accent)
    await term.waitForText("Slept.", 20_000)
    await term.waitForText(outlinedToolCard("✓", "bash", '"command":"sleep 2"'))
    const done = await at(term, "✓")
    expect((await term.lines())[done.row]).toMatch(/\d+(\.\d+)?(ms|s)/)
  })
})

test.describe("subagents", () => {
  test.use({ model: { steps: [] } })

  test("a task card shows the child's status and activity; the child opens read-only and Esc returns", async ({ tui, backend, fakeModel }, testInfo) => {
    await writeFile(join(backend.dir, "notes.txt"), "alpha\n")
    // The parent's and the child's requests are told apart by their system prompts.
    fakeModel!.route("NEVER call `report`", [
      toolStep("task", { description: "survey the repo", prompt: "list the files", subagent_type: "hya-task" }),
      textStep("Spawned a helper."),
    ])
    fakeModel!.route("Finish your task with `report`", [toolStep("read", { path: "notes.txt" }), hangStep(20_000)])
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "delegate the survey")
    await term.waitForText("Spawned a helper.", 20_000)
    // The bordered card: titled `task`, its next row names the subagent and the description.
    await term.waitForText(/┌─task[^\n]*\n[^\n]*hya-task · survey the repo/)
    // The child is still working (its model request hangs): running, with its latest tool call.
    await term.waitForText(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] running/, 15_000)
    await term.waitForText("↳ read notes.txt", 15_000)
    const status = await match(term, /[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] running/)
    expect((await term.cell(status.row, status.col))?.fg).toBe(colors.accent)
    expect((await term.cell(status.row, status.col + 2))?.fg).toBe(colors.accent)
    // The sidebar nests the child session under its parent.
    await term.waitForText("↳ 1.1 ")
    await term.attach(testInfo, "task-running")

    // A click on the card opens the child read-only.
    const card = await at(term, "hya-task · survey the repo")
    await click(term, card.row, card.col + 2)
    await term.waitForText("Viewing subagent hya-task · Esc returns")
    await term.waitForText("Read-only subagent view · / opens commands · Esc returns")
    await term.waitForText("┃ list the files")
    await term.waitForText(outlinedToolCard("✓", "read", '"path":"notes.txt"'))
    await term.attach(testInfo, "child-view")
    await prompt(term, "can I type here")
    await term.waitForText("│ can I type here")
    expect(await term.find("┃ can I type here")).toBeNull()

    // Esc returns to the parent; the typed text stays, and a second Esc clears it.
    await term.press("Escape")
    await expect.poll(() => term.find("Viewing subagent")).toBeNull()
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
    await term.waitForText("Viewing subagent hya-task · Esc returns")
    await term.press("Escape")
    await term.waitForText("Spawned a helper.")
  })
})

test.describe("narrow terminal", () => {
  test.use({ model: { permission: "allow", steps: [toolStep("bash", { command: "echo narrow-output && ls" }), textStep("Done.")] } })

  test("cards fit about 80 columns", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 480 } })
    expect((await term.size()).cols).toBeLessThanOrEqual(84)
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "run it")
    await term.waitForText("Done.", 20_000)
    await prompt(term, "/tools on")
    await term.waitForText(/│ narrow-output/)
    await term.waitForText(outlinedToolCard("✓", "bash", '"command":"echo narrow-output && ls"'))
    await term.attach(testInfo, "narrow")
  })
})
