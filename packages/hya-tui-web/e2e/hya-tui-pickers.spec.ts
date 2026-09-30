// The `/model` picker (C11; docs/tui.md "Pickers"): rows tagged by
// provider, the current value marked, filtering, a choice with a session
// open switches it at once (a notice, and the next assistant reply's header
// shows the new model), and a choice made before any session exists is
// remembered and applied to the next `CreateSession`. `/agent` opens the
// Agents view (hya-tui-agents.spec.ts).

import type { Tui } from "./harness"
import { createSession, expectStatus, expect, hyaTui, test, textStep } from "./hya"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

async function newSession(term: Tui): Promise<void> {
  await createSession(term)
}

test.describe("/model picker", () => {
  test.use({ model: { models: ["fast", "slow"], steps: [textStep("First reply."), textStep("Second reply.")] } })

  test("lists both fake models tagged by provider, the current one marked; switching updates the session and the next reply's header", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "hi")
    await term.waitForText("First reply.", 20_000)
    await term.waitForText(/● hya-main · fake\/fast/)

    await prompt(term, "/model")
    await term.waitForText("Model")
    await term.waitForText(/▸ ● fast\s+\[fake\]/)
    await term.waitForText(/ {3}slow\s+\[fake\]/)
    await term.waitForText("2 of 2")
    await term.attach(testInfo, "model-picker")

    await term.type("slow")
    await term.waitForText("1 of 2")
    await term.press("Enter")
    await expectStatus(term, "Model", "fake/slow")

    await prompt(term, "again")
    await term.waitForText("Second reply.", 20_000)
    await term.waitForText(/● hya-main · fake\/slow/)
  })

  test("before a session exists the choice is remembered and applied to the next session", async ({ tui, backend }) => {
    // --continue with no earlier session: none is open (a plain start creates one).
    const term = await tui([...hyaTui(backend), "--continue"])
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/model")
    await term.waitForText("Model")
    await term.press("ArrowDown")
    await term.press("Enter")
    await expect.poll(() => term.find("─Model")).toBeNull()
    await prompt(term, "hello")
    await term.waitForText("First reply.", 20_000)
    await term.waitForText(/● hya-main · fake\/slow/)
  })

  test("/model provider/model keeps working directly", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "/model fake/slow")
    await expectStatus(term, "Model", "fake/slow")
  })
})

test.describe("/model with the agent's model pinned in config.yaml", () => {
  test.use({ model: { models: ["fast", "slow"], agentModels: { "hya-main": "fake/slow" }, steps: [textStep("First reply."), textStep("Second reply.")] } })

  test("switches only the session; the next session starts on the pinned model again", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "/model fake/fast")
    await expectStatus(term, "Model", "fake/fast")
    await prompt(term, "hi")
    await term.waitForText("First reply.", 20_000)
    await term.waitForText(/● hya-main · fake\/fast/)

    await createSession(term)
    await prompt(term, "again")
    await term.waitForText("Second reply.", 20_000)
    await term.waitForText(/● hya-main · fake\/slow/)
  })
})
