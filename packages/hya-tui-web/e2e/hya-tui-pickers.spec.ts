// `/model` (C11) and `/agent` (C12) pickers (docs/tui.md "Pickers"): rows
// grouped/tagged by provider or the agent's default model, the current
// value marked, filtering, a choice with a session open switches it at
// once (a notice, and the next assistant reply's header shows the new
// model), and a choice made before any session exists is remembered and
// applied to the next `CreateSession`.

import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep } from "./hya"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

async function newSession(term: Tui): Promise<void> {
  await term.waitForText("Connected to hya")
  await prompt(term, "/new")
  await term.waitForText(/Created hysec_/)
}

test.describe("/model picker", () => {
  test.use({ model: { models: ["fast", "slow"], steps: [textStep("First reply."), textStep("Second reply.")] } })

  test("lists both fake models tagged by provider, the current one marked; switching updates the session and the next reply's header", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "hi")
    await term.waitForText("First reply.", 20_000)
    await term.waitForText(/● build · fake\/fast/)

    await prompt(term, "/model")
    await term.waitForText("Model")
    await term.waitForText(/▸ ● fast\s+\[fake\]/)
    await term.waitForText(/ {3}slow\s+\[fake\]/)
    await term.waitForText("2 of 2")
    await term.attach(testInfo, "model-picker")

    await term.type("slow")
    await term.waitForText("1 of 2")
    await term.press("Enter")
    await term.waitForText("Model → fake/slow")

    await prompt(term, "again")
    await term.waitForText("Second reply.", 20_000)
    await term.waitForText(/● build · fake\/slow/)
  })

  test("before a session exists the choice is remembered and applied to the next session", async ({ tui, backend }) => {
    // --continue with no earlier session: none is open (a plain start creates one).
    const term = await tui([...hyaTui(backend), "--continue"])
    await term.waitForText("Connected to hya")
    await prompt(term, "/model")
    await term.waitForText("Model")
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText("Model → fake/slow · applies when the session is created")
    await prompt(term, "hello")
    await term.waitForText("First reply.", 20_000)
    await term.waitForText(/● build · fake\/slow/)
  })

  test("/model provider/model keeps working directly", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "/model fake/slow")
    await term.waitForText("Model → fake/slow")
  })
})

test.describe("/agent picker", () => {
  test.use({ model: { steps: [textStep("Reply one."), textStep("Reply two.")] } })

  test("lists visible agents with their default model, the current one marked; switching updates the session", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "/agent")
    await term.waitForText("Agent")
    await term.waitForText(/▸ ● build\s/)
    await term.waitForText(/ {3}explore\s/)
    await term.attach(testInfo, "agent-picker")

    await term.type("explore")
    await term.press("Enter")
    await term.waitForText("Agent → explore")
    await prompt(term, "hi")
    await term.waitForText(/● explore · fake\/model/, 20_000)
  })

  test("before a session exists the choice is remembered and applied to the next session", async ({ tui, backend }) => {
    // --continue with no earlier session: none is open (a plain start creates one).
    const term = await tui([...hyaTui(backend), "--continue"])
    await term.waitForText("Connected to hya")
    await prompt(term, "/agent")
    await term.waitForText("Agent")
    await term.type("explore")
    await term.press("Enter")
    await term.waitForText("Agent → explore · applies when the session is created")
    await prompt(term, "hello")
    await term.waitForText("Reply one.", 20_000)
    await term.waitForText(/● explore · fake\/model/)
  })
})
