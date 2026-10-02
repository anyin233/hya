// The editable Projects/Conversation/right-side split tree at default and
// narrow widths, including focus and viewport behavior. See docs/tui.md "Layout".

import type { Tui } from "./harness"
import { expect, hyaTui, statusSessionId, test, textStep, toolStep, wideViewport } from "./hya"

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

  test("Projects and right-side jobs participate in the editable split tree", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 1500, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.waitForText("Projects")
    const initialConversation = (await term.find("No messages yet"))!
    const initialSessions = (await term.find("Sessions"))!
    await term.press("Alt+ArrowLeft")
    await term.waitForText("Projects")
    await term.press("Alt+ArrowRight")
    await term.type("/layout focus pane-1")
    await term.press("Enter")
    await prompt(term, "/layout focus pane-2")
    await prompt(term, "/layout resize +5")
    await expect.poll(async () => (await term.find("No messages yet"))!.col).toBeGreaterThan(initialConversation.col)
    await prompt(term, "/layout focus pane-3")
    await prompt(term, "/layout assign jobs")
    await term.waitForText("No active")
    await expect.poll(() => term.find("Sessions")).toBeNull()
    await prompt(term, "/layout focus pane-2")
    await prompt(term, "/layout assign jobs")
    await prompt(term, "/projects-sidebar off")
    await expect.poll(() => term.find("Projects")).toBeNull()
  })

  test("an open command survives a responsive pane reshape", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: { width: 1500, height: 640 } })
    await term.waitForText("Message, !shell, or @file · / commands")
    await term.type("/layout split")
    await term.waitForText("/layout split")
    await term.resize(690, 640)
    await term.waitForText("Commands")
    await term.waitForText("/layout split")
    await term.press("Escape")
    await term.waitForText("Message, !shell, or @file · / commands")
  })

  test("split, focus, and assign tiled panes; restore the saved layout on a new TUI", async ({ tui, backend }) => {
    let term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/layout split vertical jobs")
    await term.waitForText("▸ jobs · pane-8")
    const conversation = (await term.find("No messages yet"))!
    const jobs = (await term.find("jobs · pane-8"))!
    expect(jobs.col).toBeGreaterThan(conversation.col)
    await term.press("Alt+ArrowLeft")
    await prompt(term, "/layout split horizontal todos")
    await expect.poll(async () => (await term.lines()).filter((line) => line.includes("Todos")).length).toBeGreaterThanOrEqual(2)
    const inputBeforeSwap = (await term.find("Message, !shell, or @file · / commands"))!.col
    await term.type("/")
    await term.waitForText("Commands")
    await term.type("layout focus pane-8")
    await term.press("Enter")
    await prompt(term, "/layout assign composer")
    await expect.poll(async () => (await term.find("Message, !shell, or @file · / commands"))!.col).toBeGreaterThan(inputBeforeSwap)
    await prompt(term, "hello after moving conversation")
    await term.waitForText("layout reply marker l1", 20_000)

    term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("layout reply marker l1")
    await term.waitForText("jobs · pane-1")
    await prompt(term, "/layout reset")
    await expect.poll(() => term.find("pane-8")).toBeNull()
    await term.waitForText("Sessions")
  })

  test("a wide viewport shows the main column with the sidebar on the right", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    const { cols } = await term.size()
    expect(cols).toBeGreaterThanOrEqual(150)
    await prompt(term, "hello layout")
    await term.waitForText("layout reply marker l1", 20_000)

    // Sidebar: selectable Sessions stays boxed; passive sections use plain headings.
    for (const title of ["Sessions", "Todos", "Context"]) await term.waitForText(title)
    const sessions = (await term.find("Sessions"))!
    expect(sessions.col).toBeGreaterThan(cols / 2)
    expect((await term.cell(sessions.row, sessions.col - 1))?.fg).toBe(colors.border)
    expect((await term.cell(sessions.row + 1, sessions.col))?.bg).toBe(colors.panel)
    const context = (await term.find("Context"))!
    expect(context.row).toBeGreaterThan(sessions.row)
    await term.waitForText("fake/model")
    await term.waitForText(/▸ 1\. /)

    // Main column: transcript starts without a heading and uses the base background.
    expect(await term.find("hya · ")).toBeNull()
    const reply = (await term.find("layout reply marker l1"))!
    expect(reply.col).toBeLessThan(sessions.col)
    expect((await term.cell(reply.row, reply.col))?.bg).toBe(colors.bg)
    const text = await term.text()
    expect(text).not.toMatch(/─Chat─|Chat─/)
    expect(text).not.toContain("Pending")

    // Ctrl+B hides the sidebar and gives the transcript the full width; /sidebar brings it back.
    await term.press("Control+x")
    await term.type("/sidebar")
    await term.press("Enter")
    await expect.poll(() => sidebarShown(term)).toBe(false)
    await term.waitForText("layout reply marker l1")
    await prompt(term, "/sidebar")
    await expect.poll(() => sidebarShown(term)).toBe(true)
  })

  test("about 80 columns keeps the sidebar hidden without a metadata heading", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: narrow })
    await term.waitForText("Message, !shell, or @file · / commands")
    const { cols } = await term.size()
    expect(cols).toBeGreaterThanOrEqual(78)
    expect(cols).toBeLessThanOrEqual(84)
    await prompt(term, "hello narrow")
    await term.waitForText("layout reply marker l1", 20_000)
    expect(await sidebarShown(term)).toBe(false)
    await term.waitForText("Message, !shell, or @file · / commands")
    // Every row fits: no line is wider than the terminal.
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
    await term.attach(testInfo, "narrow-closed")

    await term.press("Control+x")
    await term.type("/sidebar")
    await term.press("Enter")
    expect(await term.find("mode manual")).toBeNull()
    expect(await term.find("Sessions")).toBeNull()

    await prompt(term, "/sidebar off")
    await expect.poll(() => sidebarShown(term)).toBe(false)
  })
})

test.describe("pane focus", () => {
  const longReply = `${Array.from({ length: 140 }, (_, index) => `scroll line ${index.toString().padStart(3, "0")}`).join("\n\n")}\n\nEND OF LONG REPLY`
  test.use({ model: { steps: [textStep(longReply)] } })

  test("switching panes keeps the conversation viewport where the user left it", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "show a long reply")
    await term.waitForText("END OF LONG REPLY", 20_000)
    await term.press("PageUp")
    await expect.poll(() => term.find("END OF LONG REPLY")).toBeNull()
    await term.press("Alt+ArrowRight")
    await term.press("Alt+ArrowLeft")
    expect(await term.find("END OF LONG REPLY")).toBeNull()
  })
})

test.describe("pending interactions", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo pending-block" }), textStep("after the ask")] } })

  test("an ask of the open session is a prompt; another session's ask is a compact pending block", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "run something")
    // The open session's ask: the permission prompt docked above the composer, no pending block.
    await term.waitForText("asked by hya-main", 20_000)
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
    await term.waitForText("/pending review request")
    expect(await term.find("asked by hya-main")).toBeNull()
  })
})

test.describe("jobs pane", () => {
  test.use({ model: { steps: [textStep("finished from tiled jobs pane", { chunkSize: 3, delayMs: 120 })] } })

  test("shows the open session working while its turn streams", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/layout split vertical jobs")
    await term.waitForText("▸ jobs · pane-8")
    await term.press("Alt+ArrowLeft")
    await prompt(term, "show the work")
    await term.waitForText("turn running", 20_000)
    await term.waitForText("finished from tiled jobs pane", 20_000)
  })
})
