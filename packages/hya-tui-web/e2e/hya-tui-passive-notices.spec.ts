import { expect, hyaTui, test, textStep, toolStep, toolCardBlock } from "./hya"
import type { Tui } from "./harness"

async function command(term: Tui, text: string) {
  await term.press("Control+x"); await term.type(text); await term.press("Enter")
  await expect.poll(() => term.find("Commands")).toBeNull()
}
async function noPassiveOutlines(term: Tui) {
  await expect.poll(async () => {
    const input = await term.find("Message, !shell")
    if (!input) return "input not drawn"
    return (await term.lines()).slice(0, input.row - 1).filter((line) => /[┌┐└┘╭╮╰╯]/.test(line)).join("\n")
  }).toBe("")
}
for (const width of [1100, 690]) {
  test.describe(`passive notices ${width}px`, () => {
    test.use({ model: { steps: [toolStep("bash", { command: "echo passive-warning" }), textStep("finished")] } })
    test("pending summaries and permission warnings are borderless; only the message editor has a frame", async ({ tui, backend }, testInfo) => {
      const term = await tui(hyaTui(backend), { viewport: { width, height: 640 } })
      await term.waitForText("Message, !shell")
      for (const pane of ["projects", "sessions", "todos", "context"]) await command(term, `/layout close ${pane}`)
      await term.type("ask for permission"); await term.press("Enter")
      await term.waitForText("Allow once", 20_000)
      await term.waitForText("Permission")
      await noPassiveOutlines(term)
      const option = (await term.find("▸ 1  Allow once"))!
      expect((await term.cell(option.row, option.col))?.fg).toBe("#73c8e8")
      await term.attach(testInfo, "borderless-warning")
      await command(term, "/new")
      await term.waitForText("Pending (1)")
      await noPassiveOutlines(term)
      // Reproduce the reported command while another session has a waiting request.
      await command(term, "/mew")
      await term.waitForText("Pending (1)")
      await noPassiveOutlines(term)
      await term.attach(testInfo, "borderless-pending")
      await command(term, "/pending")
      await term.waitForText("Allow once")
      await term.press("1")
      await term.waitForText(toolCardBlock("✓", "bash", '"command":"echo passive-warning"'), 20_000)
      await noPassiveOutlines(term)
    })
  })
}
