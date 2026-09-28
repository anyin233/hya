// The single-column layout (header, transcript, status line, composer,
// footer) and the toggleable sidebar (sessions, todos, context), at the
// default viewport and at about 80 columns. See docs/tui.md "Layout".

import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep, toolStep } from "./hya"

const colors = { bg: "#11151b", panel: "#1c2530", accent: "#73c8e8", border: "#405366", muted: "#9caab9" }
const narrow = { width: 690, height: 640 }

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

async function sidebarShown(term: Tui): Promise<boolean> {
  const text = await term.text()
  return text.includes("Sessions") && text.includes("Context")
}

test.describe("layout", () => {
  test.use({ model: { steps: [textStep("layout reply marker l1")] } })

  test("split, focus, and assign tiled panes; restore the saved layout on a new TUI", async ({ tui, backend }) => {
    let term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/layout split vertical jobs")
    await term.waitForText("▸ jobs · pane-2")
    const conversation = (await term.find("conversation · pane-1"))!
    const jobs = (await term.find("jobs · pane-2"))!
    expect(jobs.col).toBeGreaterThan(conversation.col)
    await term.press("Alt+ArrowLeft")
    await term.waitForText("▸ conversation · pane-1")
    await prompt(term, "/layout split horizontal todos")
    await term.waitForText("▸ todos · pane-3")
    expect((await term.find("todos · pane-3"))!.col).toBeLessThan(jobs.col)
    await prompt(term, "/layout assign conversation")
    await term.waitForText("▸ conversation · pane-3")
    await term.waitForText("todos · pane-1")

    term = await tui(hyaTui(backend))
    await term.waitForText("▸ conversation · pane-3")
    await term.waitForText("jobs · pane-2")
    await prompt(term, "/layout reset")
    await expect.poll(() => term.find("pane-3")).toBeNull()
    await term.waitForText("Sessions")
  })

  test("the default viewport shows the main column with the sidebar on the right", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    const { cols } = await term.size()
    expect(cols).toBeGreaterThanOrEqual(110)
    await prompt(term, "hello layout")
    await term.waitForText("layout reply marker l1", 20_000)

    // Sidebar: three titled boxes stacked on the right, in the panel color.
    for (const title of ["Sessions", "Todos", "Context"]) await term.waitForText(title)
    const sessions = (await term.find("Sessions"))!
    expect(sessions.col).toBeGreaterThan(cols / 2)
    expect((await term.cell(sessions.row, sessions.col - 1))?.fg).toBe(colors.border)
    expect((await term.cell(sessions.row + 1, sessions.col))?.bg).toBe(colors.panel)
    const context = (await term.find("Context"))!
    expect(context.row).toBeGreaterThan(sessions.row)
    await term.waitForText("fake/model")
    await term.waitForText(/▸ 1\. /)

    // Main column: header on row 0, transcript on the base background, no three-panel titles.
    const header = (await term.find("hya · "))!
    expect(header.row).toBe(0)
    expect(header.col).toBeLessThan(sessions.col)
    const reply = (await term.find("layout reply marker l1"))!
    expect(reply.col).toBeLessThan(sessions.col)
    expect((await term.cell(reply.row, reply.col))?.bg).toBe(colors.bg)
    const text = await term.text()
    expect(text).not.toMatch(/─Chat─|Chat─/)
    expect(text).not.toContain("Pending")

    // Ctrl+B hides the sidebar and gives the transcript the full width; /sidebar brings it back.
    await term.press("Control+b")
    await expect.poll(() => sidebarShown(term)).toBe(false)
    await term.waitForText("layout reply marker l1")
    await prompt(term, "/sidebar")
    await expect.poll(() => sidebarShown(term)).toBe(true)
    await term.waitForText("Sidebar shown · Ctrl+B toggles")
  })

  test("about 80 columns hides the sidebar until Ctrl+B or /sidebar opens it", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: narrow })
    await term.waitForText("Connected to hya")
    const { cols } = await term.size()
    expect(cols).toBeGreaterThanOrEqual(78)
    expect(cols).toBeLessThanOrEqual(84)
    await prompt(term, "hello narrow")
    await term.waitForText("layout reply marker l1", 20_000)
    expect(await sidebarShown(term)).toBe(false)
    await term.waitForText("Enter a prompt · /new creates a session · /sessions history")
    // Every row fits: no line is wider than the terminal.
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
    await term.attach(testInfo, "narrow-closed")

    await term.press("Control+b")
    await expect.poll(() => sidebarShown(term)).toBe(true)
    const sessions = (await term.find("Sessions"))!
    expect(sessions.col).toBeGreaterThan(cols / 2)
    await term.waitForText("layout reply marker l1")
    await term.attach(testInfo, "narrow-open")

    await prompt(term, "/sidebar off")
    await expect.poll(() => sidebarShown(term)).toBe(false)
    await term.waitForText("Sidebar hidden · Ctrl+B toggles")
  })
})

test.describe("pending interactions", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo pending-block" }), textStep("after the ask")] } })

  test("an ask of the open session is a prompt; another session's ask is a compact pending block", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "run something")
    // The open session's ask: the permission prompt docked above the composer, no pending block.
    await term.waitForText("asked by build", 20_000)
    const dock = (await term.find("Permission"))!
    const input = (await term.find("Message, !shell, or @file · / commands"))!
    expect(dock.row).toBeLessThan(input.row)
    expect(await term.find("Pending (1)")).toBeNull()
    // Open a new session: the first session's ask is now elsewhere, listed in the pending block.
    await prompt(term, "/new")
    await term.waitForText(/Pending \(1\)/, 20_000)
    const block = (await term.find("Pending (1)"))!
    expect(block.row).toBeLessThan((await term.find("Message, !shell, or @file · / commands"))!.row)
    expect(block.col).toBeLessThan((await term.size()).cols / 2)
    expect((await term.cell(block.row, block.col - 1))?.fg).toBe(colors.border)
    await term.waitForText(/! .*bash/)
    await term.waitForText("F4 review request")
    expect(await term.find("asked by build")).toBeNull()
  })
})

test.describe("jobs pane", () => {
  test.use({ model: { steps: [textStep("finished from tiled jobs pane", { chunkSize: 3, delayMs: 120 })] } })

  test("shows the open session working while its turn streams", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await prompt(term, "/layout split vertical jobs")
    await term.waitForText("▸ jobs · pane-2")
    await prompt(term, "show the work")
    await term.waitForText("turn running", 20_000)
    await term.waitForText("finished from tiled jobs pane", 20_000)
  })
})
