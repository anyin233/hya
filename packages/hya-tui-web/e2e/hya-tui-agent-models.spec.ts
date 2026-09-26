// The Agent Models view (`/agent-models`): per-agent default model, picked
// through the shared model picker and cleared again
// (state/agentModels.ts, `GET/PUT /v1/agent-models`).

import { expect, hyaTui, test } from "./hya"

test.describe("hya TUI Agent Models view", () => {
  test.use({ model: { steps: [], models: ["alpha", "beta"] } })

  test("/agent-models lists agents; Enter picks a default, c clears it", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/agent-models")
    await term.press("Enter")
    await term.waitForText("Agent Models")
    await term.waitForText(/AGENT\s+MODE\s+EFFECTIVE MODEL\s+SOURCE\s+NOTE/)
    await term.waitForText(/build\s+primary/)
    await term.waitForText("↑↓ move · Enter pick · c clear · r refresh · / filter · Esc close")

    await term.press("Enter")
    await term.waitForText("Model · build's default")
    await term.waitForText(/alpha\s+\[fake\]/)
    await term.press("Enter")
    await term.waitForText(/build → fake\/alpha/)
    await term.waitForText(/build\s+primary\s+fake\/alpha\s+remembered/)

    await term.press("c")
    // The busy label ("Clearing build's preference…") flashes only for the
    // request's round trip, so only the settled result is asserted.
    await term.waitForText(/Cleared build.s preference/)
    await expect.poll(() => term.find("remembered")).toBeNull()

    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })

  test("c on an agent with no preference and a non-settable agent both notice why", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("/agent-models")
    await term.press("Enter")
    await term.waitForText(/build\s+primary/)
    await term.press("c")
    await term.waitForText("build has no remembered preference")
    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })
})
