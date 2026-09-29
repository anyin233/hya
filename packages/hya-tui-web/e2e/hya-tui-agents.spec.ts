// The Agents view (`/agent`; docs/tui.md "Agents view"): primary agents,
// subagents, and system agents under titled divider rules; Enter selects a
// primary agent for the session, `m` picks an agent's default model (a
// remembered preference, or config.yaml for a pinned agent), `t` its default
// thinking effort (state/agentsView.ts, `GET/PUT /v1/agent-models`,
// `PUT /v1/agent-models/{id}/configuration`, `PUT /v1/agent-efforts/{id}`).

import { readFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, expect, hyaTui, test, textStep } from "./hya"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

async function openAgents(term: Tui): Promise<void> {
  await prompt(term, "/agent")
  await term.waitForText("── Primary agents")
}

/** Filter the view to `text` and keep the filter; the highlight lands on the first match. */
async function filterTo(term: Tui, text: string): Promise<void> {
  await term.type("/")
  await term.type(text)
  await term.press("Enter")
}

test.describe("hya TUI Agents view", () => {
  test.use({ model: { models: ["alpha", "beta"], steps: [textStep("Reply one."), textStep("Reply two.")] } })

  test("/agent shows primary, subagent, and system sections; the session's agent is marked", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/new")
    await term.waitForText(/Created hysec_/)
    await openAgents(term)
    await term.waitForText(/AGENT\s+EFFECTIVE MODEL\s+SOURCE\s+EFFORT/)
    await term.waitForText(/▸● hya-main\s+fake\//)
    await term.waitForText("── Subagents")
    await term.waitForText(/ {3}hya-scout\s+fake\//)
    await term.waitForText("── System agents")
    await term.waitForText(/ {3}hya-compaction\s/)
    await term.waitForText(/ {3}hya-title\s/)
    await term.waitForText("↑↓ move · Enter select · m model · t effort · c clear · r refresh · / filter · Esc close")
    // Sections are ordered: primary above subagents above system agents.
    const primary = await term.find("── Primary agents")
    const subagents = await term.find("── Subagents")
    const system = await term.find("── System agents")
    expect(primary!.row).toBeLessThan(subagents!.row)
    expect(subagents!.row).toBeLessThan(system!.row)
    await term.attach(testInfo, "agents-view")

    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
  })

  test("Enter selects a primary agent for the session; on a subagent it notices why", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/new")
    await term.waitForText(/Created hysec_/)

    await openAgents(term)
    await filterTo(term, "hya-scout")
    await term.press("Enter")
    await term.waitForText("hya-scout is a subagent; only primary agents run a session")
    await term.press("Escape")
    // Esc clears the filter first; wait for it so the next `/` is not read as Alt+/.
    await expect.poll(() => term.find("Filter hya-scout")).toBeNull()
    await filterTo(term, "hya-plan")
    await term.waitForText(/▸  hya-plan\s/)
    await term.press("Enter")
    await term.waitForText("Agent → hya-plan")
    await expect.poll(() => term.find("── Primary agents")).toBeNull()

    await prompt(term, "hi")
    await term.waitForText("Reply one.", 20_000)
    await term.waitForText(/● hya-plan · fake\//)
  })

  test("before a session exists Enter remembers the agent for the next session", async ({ tui, backend }) => {
    // --continue with no earlier session: none is open (a plain start creates one).
    const term = await tui([...hyaTui(backend), "--continue"])
    await term.waitForText("Connected to hya")
    await openAgents(term)
    await filterTo(term, "hya-plan")
    await term.press("Enter")
    await term.waitForText("Agent → hya-plan · applies when the session is created")
    await prompt(term, "hello")
    await term.waitForText("Reply one.", 20_000)
    await term.waitForText(/● hya-plan · fake\//)
  })

  test("m remembers a subagent's default model and c clears it", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await openAgents(term)
    await filterTo(term, "hya-scout")
    await term.press("m")
    await term.waitForText("Model · hya-scout's default")
    await term.type("beta")
    await term.press("Enter")
    await term.waitForText("hya-scout → fake/beta")
    await term.waitForText(/hya-scout\s+fake\/beta\s+remembered/)

    await term.press("c")
    await term.waitForText(/Cleared hya-scout.s preference/)
    await expect.poll(() => term.find("remembered")).toBeNull()
  })

  test("t sets an agent's default effort independent of its model; default clears it", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await openAgents(term)
    await filterTo(term, "hya-scout")
    await term.press("t")
    await term.waitForText("Thinking effort · hya-scout's default")
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText("hya-scout effort → none")
    await term.waitForText(/hya-scout\s+\S+\s+\S+\s+none \(set\)/)

    await term.press("t")
    await term.waitForText("Thinking effort · hya-scout's default")
    await term.press("ArrowUp")
    await term.press("Enter")
    await term.waitForText(/Cleared hya-scout.s effort/)
    await expect.poll(() => term.find("(set)")).toBeNull()
  })

  test("fits about 80 columns", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 480 } })
    await term.waitForText("Connected to hya")
    await openAgents(term)
    await term.waitForText("── Subagents")
    await term.waitForText(/hya-main\s+fake\//)
    await term.attach(testInfo, "agents-view-narrow")
  })
})

test.describe("hya TUI Agents view, pinned agent", () => {
  test.use({ model: { models: ["alpha", "beta"], agentModels: { "hya-main": "fake/beta" }, steps: [] } })

  test("m on a config-pinned agent writes config.yaml and applies at once; c names the file", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await openAgents(term)
    await term.waitForText(/hya-main\s+fake\/beta\s+configured/)

    await term.press("c")
    // The notice names the file (a long temp path that wraps).
    await term.waitForText("hya-main's model is pinned in")
    await term.waitForText("changes it")

    await term.press("m")
    await term.waitForText("Model · hya-main's default")
    await term.type("alpha")
    await term.press("Enter")
    await term.waitForText("hya-main → fake/alpha · saved to")
    await term.waitForText(/hya-main\s+fake\/alpha\s+configured/)
    const config = await readFile(join(backendConfigDir(backend), "config.yaml"), "utf8")
    expect(config).toContain("model: fake/alpha")
  })
})
