// Session list push (U10, docs/protocol/README.md "Session list push"): a
// TUI's sidebar and open `/sessions` picker stay current from the global
// stream without polling — a creation, rename, busy/idle flip, archive, or
// delete another client makes shows up live. Deleting the TUI's own open
// session (another client's doing) never crashes it: a notice, then a fresh
// session. `/sessions` archive-from-elsewhere is already covered by
// hya-tui-archive.spec.ts ("another client's archive marks the open row");
// this file covers the rest of the table.

import { Tui } from "./harness"
import { api, expect, fakeModelRef, hangStep, hyaTui, test, textStep, type Backend } from "./hya"

test.use({ model: { steps: [hangStep(60_000), textStep("Spare."), textStep("Spare."), textStep("Spare.")] } })

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

/** A session made by another client (an API caller, not this TUI's to drop or resume). */
async function otherSession(backend: Backend, title?: string): Promise<string> {
  const { session: created } = await api<{ session: { id: string } }>(backend, "POST", "/v1/sessions", { agent: "build", model: fakeModelRef, workdir: backend.dir })
  if (title) await api(backend, "PATCH", `/v1/sessions/${created.id}`, { title })
  return created.id
}

test("a session created, renamed, and run by another client shows up live in the sidebar", async ({ tui, backend, fakeModel }, testInfo) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText("Connected to hya", 30_000)
  await term.waitForText(/hya · hysec_\w+/)

  // Created elsewhere (`sessionStarted`, no title yet): the raw id shows up, debounced.
  // The sidebar is narrow, so a long id is truncated on screen — match its start.
  const other = await otherSession(backend)
  await term.waitForText(other.slice(0, 14), 15_000)

  // Renamed elsewhere (`sessionUpdated {title}`): the row's title updates live.
  await api(backend, "PATCH", `/v1/sessions/${other}`, { title: "Built elsewhere" })
  await term.waitForText("Built elsewhere", 15_000)
  await term.attach(testInfo, "created-and-renamed")

  // Run elsewhere (`sessionUpdated {busy}`, live-only): the row marks running, then idle again.
  await api(backend, "POST", `/v1/sessions/${other}/turns`, { prompt: { text: "a long job" } })
  await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
  await term.waitForText("build · running", 15_000)
  fakeModel!.release()
  await expect.poll(async () => (await term.text()).includes("build · running")).toBe(false)
})

test("a session deleted by another client drops its sidebar row; deleting the open one shows a notice and opens a new session", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText("Connected to hya", 30_000)
  await term.waitForText(/hya · (hysec_\w+)/)
  const openId = /hya · (hysec_\w+)/.exec(await term.text())![1]!

  // Deleted elsewhere, not the open session: the row just disappears.
  const bystander = await otherSession(backend, "Bystander")
  await term.waitForText("Bystander", 15_000)
  await api(backend, "DELETE", `/v1/sessions/${bystander}`)
  await expect.poll(async () => (await term.text()).includes("Bystander")).toBe(false)

  // Deleted elsewhere while open: a notice, then a fresh session — never a crash.
  await api(backend, "DELETE", `/v1/sessions/${openId}`)
  await term.waitForText(`Session ${openId} was deleted elsewhere; opened a new session`, 15_000)
  await expect.poll(async () => /hya · (hysec_\w+)/.exec(await term.text())?.[1]).not.toBe(openId)
  await term.attach(testInfo, "open-session-deleted")
})

test("the /sessions picker hint fits at 80 columns", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 640 } })
  const { cols } = await term.size()
  expect(cols).toBeLessThanOrEqual(84)
  await term.waitForText("Connected to hya")
  await prompt(term, "/sessions")
  await term.waitForText("Sessions")
  await term.waitForText("Esc closes")
  for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
  await term.attach(testInfo, "sessions-hint-80-cols")
})
