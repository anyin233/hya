// The Saved Rules view (`/rules`): a "always allow" answer to a bash ask
// persists a saved rule (state/rules.ts, `GET /v1/permissions/rules`); the
// view lists it and deletes it with `d` + Enter.

import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep, toolStep } from "./hya"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

test.describe("hya TUI Saved Rules view", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo saved-rule" }), textStep("Ran it.")] } })

  test("a bash always-allow answer is a saved rule; /rules lists and deletes it", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "run it")
    await term.waitForText("$ echo saved-rule", 20_000)
    await term.press("2") // Always allow
    await term.waitForText("Ran it.", 20_000)

    await prompt(term, "/rules")
    await term.waitForText("Saved Rules")
    await term.waitForText(/EFFECT\s+TOOL\s+PATTERN\s+SAVED/)
    await term.waitForText(/allow\s+bash/)
    await term.waitForText("↑↓ move · d delete · r refresh · / filter · Esc close")

    await term.press("d")
    await term.waitForText(/Delete bash .*\? Enter confirms · Esc cancels/)
    await term.press("Enter")
    await term.waitForText(/Deleted rule/)
    await expect.poll(async () => /allow\s+bash/.test(await term.text())).toBe(false)

    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })

  test("Esc during a pending delete cancels it; r refreshes the list", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "run it")
    await term.waitForText("$ echo saved-rule", 20_000)
    await term.press("2")
    await term.waitForText("Ran it.", 20_000)

    await prompt(term, "/rules")
    await term.waitForText(/allow\s+bash/)
    await term.press("d")
    await term.waitForText("Enter confirms · Esc cancels")
    await term.press("Escape")
    // A real gap before the next key: a lone Esc immediately followed by
    // another key can be read as one Alt+<key> chord (the terminal's own
    // escape-sequence ambiguity; docs/tui.md notes the same for Alt+Enter).
    await expect.poll(() => term.find("Enter confirms")).toBeNull()
    await term.press("r")
    await term.waitForText("Refreshed")
    await term.press("Escape")
  })
})
