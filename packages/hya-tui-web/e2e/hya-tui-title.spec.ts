// Terminal title (docs/tui.md "Terminal title", docs/tui-web.md "Tab title",
// ADR-0030): the TUI names its terminal after the open session and marks
// what it needs — `●` a turn runs, `?` an ask waits, `✓`/`✗` a turn ended
// while unfocused — over OSC 0, only when the title changes. This spec
// records every title the emulator reports (xterm.js `onTitleChange`) and
// separately checks the WebUI host's generic mapping to `document.title`.
//
// As in e2e/hya-tui-notifications.spec.ts, prompts are typed while focused
// (keys need focus) and the terminal is blurred afterwards.

import { terminalDriver, type Tui } from "./harness"
import { expect, hyaTui, test, textStep, toolStep } from "./hya"

/** Record every title the terminal reports from now on. */
async function captureTitles(term: Tui): Promise<void> {
  await term.inspect((terminal, state) => {
    const view = state as { titles?: string[] }
    view.titles = []
    terminal.onTitleChange((title: string) => { view.titles!.push(title) })
  })
}

async function titles(term: Tui): Promise<string[]> {
  return term.inspect((_terminal, state) => (state as { titles?: string[] }).titles ?? [])
}

async function lastTitle(term: Tui): Promise<string> {
  return (await titles(term)).at(-1) ?? ""
}

async function prompt(term: Tui, text: string, { blur = false } = {}): Promise<void> {
  await term.focus(true)
  await term.type(text)
  await term.press("Enter")
  if (blur) await term.focus(false)
}

test.describe("terminal title: turn activity", () => {
  test.use({ model: { steps: [textStep("Slow title reply.", { chunkSize: 2, delayMs: 60 }), textStep("Unseen reply.", { chunkSize: 4, delayMs: 30 })] } })

  test("marks a running turn, and an unseen outcome until focus", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")

    // A running turn is marked `●`; idle again drops the marker.
    await captureTitles(term)
    await prompt(term, "say it slowly")
    await expect.poll(async () => (await titles(term)).some((title) => title.startsWith("● ")), { timeout: 20_000 }).toBe(true)
    await term.waitForText("Slow title reply.", 20_000)
    await term.waitForIdle()
    await expect.poll(() => lastTitle(term)).toMatch(/^[^●?✓✗]/)

    // Ended while unfocused: `✓` until the terminal is focused again.
    await prompt(term, "say it while away", { blur: true })
    await term.waitForText("Unseen reply.", 20_000)
    await expect.poll(() => lastTitle(term), { timeout: 20_000 }).toMatch(/^✓ /)
    await term.focus(true)
    await expect.poll(() => lastTitle(term)).toMatch(/^[^●?✓✗]/)

  })
})

test.describe("terminal title: permission ask", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo titled" }), textStep("Ran it.")] } })

  test("marks a waiting permission ask with ?", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await captureTitles(term)
    await prompt(term, "run the command")
    await expect.poll(() => lastTitle(term), { timeout: 20_000 }).toMatch(/^\? /)
  })
})

test.describe("terminal title: WebUI tab", () => {
  test.skip(terminalDriver === "pty", "document.title needs the browser page")
  test.use({ model: { steps: [textStep("Tab title reply.", { chunkSize: 2, delayMs: 60 })] } })

  test("the browser tab title follows the terminal title", async ({ tui, backend, page }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "say it in the tab")
    await expect.poll(() => page.title(), { timeout: 20_000 }).toMatch(/^● /)
    await term.waitForIdle()
    await expect.poll(() => page.title()).toMatch(/^[^●?✓✗]/)
  })
})
