// Re-entering a conversation that was archived while a permission check waits.
// The transcript and the request must be reachable without copying a session
// or interaction ID from the narrow pending box.

import type { Tui } from "./harness"
import { api, expect, fakeModelRef, hyaTui, toolCardBlock, statusSessionId, test, textStep, toolStep } from "./hya"

async function prompt(term: Tui, value: string): Promise<void> {
  await term.type(value)
  await term.waitForText(value)
  await term.press("Enter")
}

test.use({ model: { steps: [toolStep("bash", { command: "git status --short --branch" }), textStep("Repository status checked.")] } })

test("a plain relaunch restores the waiting chat and offers approval keys, then keeps its history", async ({ tui, backend }, testInfo) => {
  const first = await tui(hyaTui(backend), { viewport: { width: 690, height: 480 } })
  await first.waitForText("Message, !shell, or @file · / commands")
  const id = await statusSessionId(first)
  await prompt(first, "Check this repository")
  await first.waitForText(toolCardBlock("◌", "bash", '"command":"git status --short --branch"', "awaiting approval"), 20_000)
  await prompt(first, "/exit")
  expect(await first.waitForExit()).toBe(0)
  const archived = await api<{ archived?: boolean; busy?: boolean }>(backend, "GET", `/v1/sessions/${id}`)
  expect(archived.archived).toBe(true)
  expect(archived.busy).toBe(true)

  // A newer saved chat must not hide a conversation that needs an answer.
  const { session: newer } = await api<{ session: { id: string } }>(backend, "POST", "/v1/sessions", {
    agent: "hya-main", model: fakeModelRef, workdir: backend.dir,
  })
  await api(backend, "PATCH", `/v1/sessions/${newer.id}`, { title: "Newer saved chat" })

  const second = await tui(hyaTui(backend), { viewport: { width: 690, height: 480 } })
  await second.waitForText("Check this repository", 20_000)
  expect(await second.text()).not.toContain("Newer saved chat")
  await second.waitForText(toolCardBlock("◌", "bash", '"command":"git status --short --branch"', "awaiting approval"), 20_000)
  await second.waitForText(/1 {2}Allow once/)
  await second.waitForText(/2 {2}Always allow/)
  await second.waitForText(/3 {2}Deny/)
  expect((await api<{ id: string; archived?: boolean }>(backend, "GET", `/v1/sessions/${id}`)).archived ?? false).toBe(false)
  await second.attach(testInfo, "reopened-permission")
  await second.press("1")
  await second.waitForText("Repository status checked.", 20_000)
  await prompt(second, "/exit")
  expect(await second.waitForExit()).toBe(0)

  const third = await tui(hyaTui(backend), { viewport: { width: 690, height: 480 } })
  await third.waitForText("Check this repository", 20_000)
  await third.waitForText("Repository status checked.", 20_000)
  await third.attach(testInfo, "reopened-history")
})
