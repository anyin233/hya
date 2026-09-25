// Message rendering: user vs assistant styling, Markdown with highlighted
// code blocks, collapsible reasoning, error and finish notices, and
// transcript scrolling with the "new messages below" hint. See docs/tui.md
// "Messages".

import type { Tui } from "./harness"
import { expect, hangStep, httpErrorStep, hyaTui, reasoningStep, test, textStep } from "./hya"

const colors = {
  bg: "#11151b", panel: "#1c2530", fg: "#e8edf3", muted: "#9caab9", accent: "#73c8e8",
  error: "#f07878", warning: "#e5c07b", keyword: "#c792ea", string: "#a5d6a7", inlineCode: "#f2a97a",
}

/** The status line reads exactly `Ready` (the sidebar may share its screen row). */
const readyLine = /^Ready(?! ·)/m

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

async function at(term: Tui, needle: string) {
  const found = await term.find(needle)
  expect(found, `screen shows ${needle}`).not.toBeNull()
  return found!
}

test.describe("roles", () => {
  test.use({ model: { steps: [textStep("Plain answer from the assistant")] } })

  test("user messages are panel blocks with an accent bar; assistant messages have an agent · model header", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "question from the user")
    await term.waitForText("Plain answer from the assistant", 20_000)
    await term.waitForText(readyLine)

    const user = await at(term, "question from the user")
    const bar = await term.cell(user.row, user.col - 2)
    expect(bar?.char).toBe("┃")
    expect(bar?.fg).toBe(colors.accent)
    expect((await term.cell(user.row, user.col))?.bg).toBe(colors.panel)
    expect((await term.cell(user.row, user.col))?.fg).toBe(colors.fg)

    const header = await at(term, "build · fake/model")
    expect(header.row).toBeGreaterThan(user.row)
    expect((await term.cell(header.row, header.col))?.fg).toBe(colors.accent)
    expect((await term.cell(header.row, header.col + "build · ".length))?.fg).toBe(colors.muted)
    const answer = await at(term, "Plain answer from the assistant")
    expect(answer.row).toBe(header.row + 1)
    expect((await term.cell(answer.row, answer.col))?.bg).toBe(colors.bg)

    // No role/finish text headers: a plain stop is not noteworthy.
    const text = await term.text()
    expect(text).not.toContain("user · stop")
    expect(text).not.toContain("assistant · stop")
    expect(text).not.toMatch(/\bstop\b/)
  })
})

test.describe("markdown", () => {
  const reply = [
    "# Release plan",
    "",
    "Ship the **bold** change and run `bun test` first.",
    "",
    "- first item",
    "  - nested item",
    "",
    "```ts",
    "const answer = \"forty-two\"",
    "```",
    "",
    "Done.",
  ].join("\n")
  test.use({ model: { steps: [textStep(reply, { chunkSize: 16, delayMs: 20 })] } })

  test("renders headings, emphasis, inline code, lists, and a highlighted code block", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "show markdown")
    await term.waitForText("Done.", 20_000)
    await term.waitForText(readyLine)

    const heading = await at(term, "Release plan")
    await expect.poll(async () => (await term.cell(heading.row, heading.col))?.fg).toBe(colors.accent)
    expect((await term.cell(heading.row, heading.col))?.bold).toBe(true)
    expect(await term.find("# Release plan")).toBeNull()

    const bold = await at(term, "bold change")
    expect((await term.cell(bold.row, bold.col))?.bold).toBe(true)
    expect(await term.find("**bold**")).toBeNull()
    const code = await at(term, "bun test")
    expect((await term.cell(code.row, code.col))?.fg).toBe(colors.inlineCode)

    const nested = await at(term, "- nested item")
    const first = await at(term, "- first item")
    expect(nested.col).toBe(first.col + 2)

    const keyword = await at(term, "const answer")
    await expect.poll(async () => (await term.cell(keyword.row, keyword.col))?.fg).toBe(colors.keyword)
    expect((await term.cell(keyword.row, keyword.col))?.bg).toBe(colors.panel)
    const string = await at(term, "\"forty-two\"")
    expect((await term.cell(string.row, string.col))?.fg).toBe(colors.string)
    expect(await term.find("```")).toBeNull()
  })
})

test.describe("reasoning", () => {
  test.use({
    model: {
      protocol: "responses",
      steps: [reasoningStep("private chain of thought about apples", "The final answer is 7.", { chunkSize: 12, delayMs: 30 })],
    },
  })

  test("reasoning is a collapsed Thinking line that Ctrl+O and /thinking expand", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "think about it")
    await term.waitForText("The final answer is 7.", 20_000)
    await term.waitForText(readyLine)

    const label = await at(term, "▸ Thinking · 6 words")
    expect((await term.cell(label.row, label.col))?.fg).toBe(colors.muted)
    expect(await term.find("private chain of thought")).toBeNull()
    expect(label.row).toBeLessThan((await at(term, "The final answer is 7.")).row)

    await term.press("Control+o")
    await term.waitForText("▾ Thinking · 6 words")
    const body = await at(term, "private chain of thought about apples")
    expect((await term.cell(body.row, body.col))?.fg).toBe(colors.muted)
    expect((await term.cell(body.row, body.col))?.italic).toBe(true)

    await term.press("Control+o")
    await term.waitForText("▸ Thinking · 6 words")
    expect(await term.find("private chain of thought")).toBeNull()

    await prompt(term, "/thinking")
    await term.waitForText("private chain of thought about apples")
    await term.waitForText("Reasoning expanded · Ctrl+O toggles")

    // A click on one Thinking line toggles just that block.
    const line = await at(term, "▾ Thinking")
    const screen = (await term.page.locator(".xterm-screen").boundingBox())!
    const { cols, rows } = await term.size()
    await term.page.mouse.click(screen.x + ((line.col + 3) / cols) * screen.width, screen.y + ((line.row + 0.5) / rows) * screen.height)
    await term.waitForText("▸ Thinking · 6 words")
    expect(await term.find("private chain of thought")).toBeNull()
    // The click leaves the input focused: typing still reaches it.
    await prompt(term, "/thinking off")
    await term.waitForText("Reasoning collapsed · Ctrl+O toggles")
  })
})

test.describe("errors and notices", () => {
  test.use({ model: { steps: [httpErrorStep(400), textStep("cut off mid", { finish: "length" }), hangStep()] } })

  test("a failed turn shows a red error line; length and cancel show notices", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "please fail")
    await term.waitForText("✗ provider_error: http status 400", 20_000)
    const error = await at(term, "✗ provider_error")
    expect((await term.cell(error.row, error.col))?.fg).toBe(colors.error)
    expect((await term.cell(error.row, error.col + 2))?.fg).toBe(colors.error)

    await prompt(term, "please stop early")
    await term.waitForText("! Reply stopped at the output length limit", 20_000)
    const length = await at(term, "! Reply stopped")
    expect((await term.cell(length.row, length.col))?.fg).toBe(colors.warning)

    await prompt(term, "please hang")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await term.waitForText("Esc to interrupt")
    await prompt(term, "/cancel")
    await term.waitForText("! Cancelled", 20_000)
    const cancelled = await at(term, "! Cancelled")
    expect((await term.cell(cancelled.row, cancelled.col))?.fg).toBe(colors.warning)
  })
})

test.describe("scrolling", () => {
  const lines = Array.from({ length: 90 }, (_, index) => `row ${String(index + 1).padStart(2, "0")} of the long reply`)
  // About 5 rows per chunk, one chunk every 250 ms: the reply streams for ~4.5 s.
  test.use({ model: { steps: [textStep(lines.join("\n\n"), { chunkSize: 130, delayMs: 250 })] } })

  test("PgUp/PgDn, End, and the mouse wheel scroll; new content below a scrolled-up view shows a hint", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "long reply please")
    // Following the bottom while it streams.
    await term.waitForText("row 20 of the long reply", 20_000)
    await term.waitForText("row 24 of the long reply")

    await term.press("PageUp")
    await expect.poll(async () => (await term.text()).includes("row 24 of the long reply")).toBe(false)
    await term.waitForText("↓ New messages below", 10_000)
    const hint = await at(term, "↓ New messages below")
    expect((await term.cell(hint.row, hint.col))?.fg).toBe(colors.accent)
    await term.attach(testInfo, "scrolled-up-hint")

    // The reply finishes while scrolled up: the view stays put.
    await expect.poll(async () => (await term.text()).includes("Ready"), { timeout: 20_000 }).toBe(true)
    expect(await term.find("row 90 of the long reply")).toBeNull()

    await term.press("End")
    await term.waitForText("row 90 of the long reply")
    await expect.poll(async () => (await term.text()).includes("New messages below")).toBe(false)

    await term.press("PageUp")
    await term.press("PageUp")
    await expect.poll(async () => (await term.text()).includes("row 90 of the long reply")).toBe(false)
    await term.press("PageDown")
    await term.press("PageDown")
    await term.press("PageDown")
    await term.waitForText("row 90 of the long reply")

    const row = await at(term, "row 90 of the long reply")
    const box = (await term.page.locator(".xterm-screen").boundingBox())!
    const cell = await term.size()
    await term.page.mouse.move(box.x + box.width / 4, box.y + ((row.row + 0.5) / cell.rows) * box.height)
    await term.page.mouse.wheel(0, -600)
    await expect.poll(async () => (await term.text()).includes("row 90 of the long reply")).toBe(false)
    await term.press("Control+End")
    await term.waitForText("row 90 of the long reply")
  })
})
