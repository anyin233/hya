// Permission modes (docs/tui.md "Permission modes"): Shift+Tab cycling
// through xterm.js (CSI Z), the one-time yolo confirmation, the status bar
// mode colors, the transcript notice, yolo closing a pending ask, the
// `/permissions` picker (a reusable modal list), a mode chosen before any
// session exists, and a bundle mode whose Bun `permission.approve` hook
// decides — all under the default permission model, where bash asks.

import type { Tui } from "./harness"
import { approverBundle, expect, hyaTui, test, textStep, toolStep, type Backend } from "./hya"

const colors = { fg: "#e8edf3", muted: "#9caab9", accent: "#73c8e8", error: "#f07878", warning: "#e5c07b" }
const narrow = { width: 690, height: 640 }
const confirmLine = "Enable yolo? Every tool call runs without asking · Enter confirms · Esc cancels"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.press("Enter")
}

async function at(term: Tui, needle: string) {
  const found = await term.find(needle)
  expect(found, `screen shows ${needle}`).not.toBeNull()
  return found!
}

/** Create a session with `/new` and wait until it is open. */
async function newSession(term: Tui): Promise<void> {
  await term.waitForText("Connected to hya")
  await prompt(term, "/new")
  await term.waitForText(/Created hysec_/)
}

/** Esc closes the picker; wait for it, so the next keys are not read as Alt+key after a lone ESC. */
async function closePicker(term: Tui): Promise<void> {
  await term.press("Escape")
  await expect.poll(async () => (await term.text()).includes("Filter ")).toBe(false)
}

async function promptGone(term: Tui): Promise<void> {
  await expect.poll(async () => /asked by /.test(await term.text()), { timeout: 20_000 }).toBe(false)
}

/** The first row showing `mode …` (the status bar). */
async function statusBar(term: Tui): Promise<{ row: number; line: string }> {
  const lines = await term.lines()
  const row = lines.findIndex((line) => line.startsWith("mode "))
  expect(row, "status bar row").toBeGreaterThanOrEqual(0)
  return { row, line: lines[row]! }
}

/** `permissionMode` of the backend's only top-level session. */
async function backendMode(backend: Backend): Promise<string | undefined> {
  const response = await fetch(`${backend.url}/v1/sessions`, { headers: { "x-hya-directory": backend.dir } })
  const body = await response.json() as { sessions?: { parent?: string; permissionMode?: string }[] }
  return body.sessions?.find((session) => !session.parent)?.permissionMode
}

test.describe("Shift+Tab switching", () => {
  test.use({
    model: {
      steps: [
        toolStep("bash", { command: "echo yolo-run" }),
        textStep("Ran without asking."),
        toolStep("bash", { command: "echo manual-run" }),
        textStep("Asked again."),
      ],
    },
  })

  test("Shift+Tab asks before yolo; confirmed yolo runs bash without a prompt; manual asks again", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    expect((await statusBar(term)).line).toMatch(/^mode manual/)

    // Shift+Tab reaches the TUI through xterm.js (CSI Z) and asks first.
    await term.press("Shift+Tab")
    await term.waitForText(confirmLine)
    const warn = await at(term, "⚠ Enable yolo?")
    expect((await term.cell(warn.row, warn.col))?.fg).toBe(colors.error)
    expect((await statusBar(term)).line).toMatch(/^mode manual/)
    await term.attach(testInfo, "yolo-confirm")
    // Esc keeps manual.
    await term.press("Escape")
    await term.waitForText("Permission mode unchanged · manual")
    expect(await term.text()).not.toContain(confirmLine)
    expect(await backendMode(backend)).toBe("manual")

    // Shift+Tab again, Enter confirms.
    await term.press("Shift+Tab")
    await term.waitForText(confirmLine)
    await term.press("Enter")
    await term.waitForText("mode ⚠ yolo")
    await term.waitForText("Permission mode → yolo")
    const bar = await statusBar(term)
    const yolo = bar.line.indexOf("⚠ yolo")
    expect((await term.cell(bar.row, yolo))?.fg).toBe(colors.error)
    expect((await term.cell(bar.row, yolo + 2))?.fg).toBe(colors.error)
    expect((await term.cell(bar.row, 0))?.fg).toBe(colors.muted)
    await expect.poll(() => backendMode(backend)).toBe("yolo")
    // The notice is a muted transcript line.
    const notice = await at(term, "Permission mode → yolo")
    expect((await term.cell(notice.row, notice.col))?.fg).toBe(colors.muted)

    // Under yolo the bash call runs at once: no prompt.
    await prompt(term, "run it under yolo")
    await term.waitForText("Ran without asking.", 20_000)
    await term.waitForText(/✓ bash\s+echo yolo-run/)
    expect(await term.text()).not.toContain("asked by")

    // Shift+Tab back to manual (no confirmation needed); the next bash asks.
    await term.press("Shift+Tab")
    await term.waitForText(/^mode manual/m)
    await term.waitForText("Permission mode → manual")
    await expect.poll(() => backendMode(backend)).toBe("manual")
    await prompt(term, "run it under manual")
    await term.waitForText("asked by build", 20_000)
    // The prompt's hint row names the mode.
    await term.waitForText(/Esc denies · perm_\w+ · mode manual/)
    await term.press("1")
    await term.waitForText("Asked again.", 20_000)
    await promptGone(term)

    // Once confirmed, yolo no longer asks in this TUI process.
    await term.press("Shift+Tab")
    await term.waitForText("mode ⚠ yolo")
    expect(await term.text()).not.toContain(confirmLine)
  })
})

test.describe("yolo with a pending ask", () => {
  test.use({ model: { steps: [toolStep("bash", { command: "echo pending-ask" }), textStep("Continued after yolo.")] } })

  test("switching to yolo closes the pending bash prompt and the tool completes", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "run the command")
    await term.waitForText("asked by build", 20_000)
    await term.waitForText(/◌ bash\s+echo pending-ask · awaiting approval/)
    // Shift+Tab works with the prompt shown; the confirmation takes Enter, not the prompt.
    await term.press("Shift+Tab")
    await term.waitForText(confirmLine)
    await term.attach(testInfo, "yolo-confirm-over-prompt")
    await term.press("Enter")
    await term.waitForText("mode ⚠ yolo")
    await promptGone(term)
    await term.waitForText("Continued after yolo.", 20_000)
    await term.waitForText(/✓ bash\s+echo pending-ask/)
    // The ask was allowed by the switch, not by the prompt's Enter (Allow once).
    expect(await term.text()).not.toContain("Allowed once")
  })
})

test.describe("/permissions picker", () => {
  test("lists the modes with sources, filters, selects, and gives the focus back to the input", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "/permissions")
    await term.waitForText("Permission mode")
    await term.waitForText(/▸ ● Manual\s+\[builtin\]\s+Ask the user before actions/)
    await term.waitForText(/ {3}Yolo\s+\[builtin\]\s+Allow every action/)
    await term.waitForText("2 of 2")
    await term.waitForText("↑↓ select · Enter chooses · Esc closes · type to filter")
    const highlighted = await at(term, "▸ ● Manual")
    expect((await term.cell(highlighted.row, highlighted.col))?.fg).toBe(colors.accent)
    await term.attach(testInfo, "picker")

    // Typing filters (the input does not receive the text).
    await term.type("yo")
    await term.waitForText("1 of 2")
    expect(await term.text()).not.toMatch(/Manual\s+\[builtin\]/)
    await term.press("Backspace")
    await term.press("Backspace")
    await term.waitForText("2 of 2")
    // Down/Up and Shift+Tab move the highlight; Esc closes without a change.
    await term.press("ArrowDown")
    await term.waitForText(/▸ {3}Yolo/)
    await term.press("Shift+Tab")
    await term.waitForText(/▸ ● Manual/)
    await closePicker(term)
    expect((await statusBar(term)).line).toMatch(/^mode manual/)

    // The input has the focus again.
    await term.type("typed after")
    await term.waitForText("│ typed after")
    for (let index = 0; index < "typed after".length; index++) await term.press("Backspace")

    // Enter chooses the highlighted row; yolo still asks first.
    await prompt(term, "/permissions")
    await term.waitForText("Filter ")
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText(confirmLine)
    await term.press("Enter")
    await term.waitForText("mode ⚠ yolo")
    await expect.poll(() => backendMode(backend)).toBe("yolo")

    // Reopened, the current mode is marked; `/permissions manual` switches directly.
    await prompt(term, "/permissions")
    await term.waitForText(/▸ ● Yolo/)
    await closePicker(term)
    await prompt(term, "/permissions manual")
    await term.waitForText(/^mode manual/m)
    await expect.poll(() => backendMode(backend)).toBe("manual")
  })

  test("Shift+Tab moves up in the open command menu instead of switching the mode", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await term.type("/")
    await term.waitForText("Commands")
    await term.press("Shift+Tab")
    expect(await term.text()).not.toContain(confirmLine)
    expect((await statusBar(term)).line).toMatch(/^mode manual/)
    // The highlight wrapped to the last entry: the first entry is no longer marked.
    await expect.poll(async () => (await term.lines()).filter((line) => line.includes("▸ /")).length).toBe(1)
    await term.press("Escape")
  })

  test("fits about 80 columns", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: narrow })
    await newSession(term)
    const { cols } = await term.size()
    expect(cols).toBeLessThanOrEqual(84)
    await prompt(term, "/permissions")
    await term.waitForText(/▸ ● Manual\s+\[builtin\]/)
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
    await term.attach(testInfo, "narrow-picker")
    await closePicker(term)
    await term.press("Shift+Tab")
    await term.waitForText("⚠ Enable yolo?")
    await term.press("Enter")
    await term.waitForText("mode ⚠ yolo")
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
  })
})

test.describe("before a session exists", () => {
  test("the chosen mode is shown and applied right after the session is created", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.press("Shift+Tab")
    await term.waitForText(confirmLine)
    await term.press("Enter")
    await term.waitForText("Permission mode → yolo · applies when the session is created")
    await term.waitForText("mode ⚠ yolo")
    await prompt(term, "hello")
    await term.waitForText(/^Ready/m, 20_000)
    await expect.poll(() => backendMode(backend)).toBe("yolo")
    await term.waitForText("Permission mode → yolo")
    await term.waitForText("mode ⚠ yolo")
  })
})

test.describe("bundle permission mode", () => {
  test.use({
    projectBundles: {
      approver: approverBundle({
        id: "e2e/approver",
        modes: [{ id: "echo-only", title: "Echo only", description: "Approve echo commands; ask for the rest" }],
        approve: `async ({ mode, action, resource }) =>
      mode === "echo-only" && action === "bash" && /^echo /.test(String(resource?.value ?? "")) ? "allow_once" : "defer"`,
      }),
    },
    model: {
      steps: [
        toolStep("bash", { command: "echo approved-by-plugin" }),
        textStep("Echo went through."),
        toolStep("bash", { command: "ls" }),
        textStep("Listed after asking."),
      ],
    },
  })

  test("the picker lists the bundle's mode with its source; when active its approver decides", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await newSession(term)
    await prompt(term, "/permissions")
    await term.waitForText(/Echo only\s+\[e2e\/approver\]\s+Approve echo commands/, 20_000)
    await term.waitForText("3 of 3")
    await term.type("echo")
    await term.waitForText("1 of 3")
    await term.press("Enter")
    // A bundle mode needs no confirmation; the status bar shows its title in the accent color.
    await term.waitForText("mode Echo only")
    await term.waitForText("Permission mode → Echo only (e2e/approver/echo-only)")
    const bar = await statusBar(term)
    expect((await term.cell(bar.row, bar.line.indexOf("Echo only")))?.fg).toBe(colors.accent)
    await expect.poll(() => backendMode(backend)).toBe("e2e/approver/echo-only")

    // The approver allows the echo command: no prompt.
    await prompt(term, "echo something")
    await term.waitForText("Echo went through.", 30_000)
    await term.waitForText(/✓ bash\s+echo approved-by-plugin/)
    expect(await term.text()).not.toContain("asked by")

    // It defers anything else: the user is asked.
    await prompt(term, "list files")
    await term.waitForText("asked by build", 20_000)
    await term.waitForText(/Esc denies · perm_\w+ · mode Echo only/)
    await term.attach(testInfo, "bundle-mode-ask")
    await term.press("1")
    await term.waitForText("Listed after asking.", 20_000)
    await promptGone(term)

    // Shift+Tab from the bundle mode wraps around to manual.
    await term.press("Shift+Tab")
    await term.waitForText(/^mode manual/m)
  })
})
