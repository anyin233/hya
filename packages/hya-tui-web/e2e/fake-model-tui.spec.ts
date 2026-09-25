// Drives the real TUI against the isolated backend wired to the scripted
// fake model, so later Tier 1 specs (streaming render, tool cards, busy
// state) have a model that can produce more than the offline echo's canned
// reply. Assertions stay loose (see AGENTS.md "TUI Preview & Browser Test
// Rule") because a concurrent step is refactoring the TUI's internals, not
// its on-screen text.

import { expect, hyaTui, textStep, test } from "./hya"

test.describe("hya TUI against the fake model", () => {
  test.use({ model: { steps: [textStep("the fake model replies with marker hya-fake-b7d2")] } })

  test("shows a scripted fake-model reply on screen", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("say something")
    await term.press("Enter")
    await term.waitForText("hya-fake-b7d2", 20_000)
    await term.waitForText("● build · fake/model")
    expect(await term.find("┃ say something")).not.toBeNull()
  })
})
