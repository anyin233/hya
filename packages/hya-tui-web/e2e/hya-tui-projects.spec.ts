// The left Projects sidebar and the full-screen Project view (docs/tui.md
// "Projects"): the sidebar's live list with a busy marker and switching, the
// view's create/edit-roots/rename/delete flows, `--remote` opening the view
// automatically, and the `/sessions` picker's project scoping and
// "all projects" toggle.

import { mkdtemp } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { Tui } from "./harness"
import { api, expect, expectStatus, statusSessionId, hyaTui, test, textStep, tuiMain } from "./hya"

const narrow = { width: 690, height: 640 }
const wide = { width: 1500, height: 640 }

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

/** Esc, then wait for it to take effect before the next key (see other specs: a lone Esc followed at
 * once by another key can be read as an Alt+key chord by the terminal). */
async function esc(term: Tui, thenGone: string | RegExp): Promise<void> {
  await term.press("Escape")
  await expect.poll(() => term.find(typeof thenGone === "string" ? thenGone : "")).toBeNull()
}

/** Center of one terminal cell in page pixels (see hya-tui-sidebar-resize.spec.ts). */
async function cellPoint(term: Tui, row: number, col: number): Promise<{ x: number; y: number }> {
  const box = await term.screenBox()
  const { cols, rows } = await term.size()
  return { x: box.x + ((col + 0.5) / cols) * box.width, y: box.y + ((row + 0.5) / rows) * box.height }
}

test.describe("Projects sidebar", () => {
  test.use({ model: { steps: [textStep("first reply"), textStep("second reply")] } })

  test("hidden at the default viewport (both sidebars would not fit next to the chat column)", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    expect(await term.find("Projects")).toBeNull()
  })

  test("a wide terminal shows every Project, the active one and a session count; Enter switches", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "hello")
    await term.waitForText("first reply", 20_000)
    await term.waitForText("Projects")
    // The ensured Project (named after the workspace directory) is listed with a session count.
    const projectsTitle = (await term.find("Projects"))!
    expect(projectsTitle.col).toBeLessThan((await term.size()).cols / 2)
    await term.waitForText(/\(1\)/)
    await term.attach(testInfo, "sidebar-wide")

    // Create a second Project via /project, then switch to it from the sidebar with Alt+0, Down, Enter.
    await prompt(term, "/project")
    await term.waitForText("Projects")
    await term.waitForText("Up/Down move · Enter opens/switches")
    await term.press("n")
    await term.waitForText("New project name")
    await term.type("second")
    await term.press("Enter")
    await term.waitForText("Root 1 (primary)")
    const secondRoot = await mkdtemp(join(tmpdir(), "hya-e2e-second-"))
    await term.type(secondRoot)
    await term.press("Enter")
    await term.waitForText("Root 2 (Enter empty to finish)")
    await term.press("Enter")
    await term.waitForText(/Created second/)
    await esc(term, "Created second")

    // The Projects sidebar may cut the name (`seco… (0)`).
    await term.waitForText(/seco(?:nd|…) \(0\)/)
    await term.type("/projects-sidebar on")
    await term.press("Enter")
    await term.type("/layout focus pane-2")
    await term.press("Enter")
    await term.press("ArrowDown")
    await term.press("Enter")
    await expectStatus(term, "Directory", secondRoot)
  })

  test("a rule separates the Projects and a mouse click on a row switches to it", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "hello")
    await term.waitForText("first reply", 20_000)

    // A second Project arrives through the API; the sidebar list follows live.
    // The narrow pane may truncate its name (`seco…`).
    const secondRoot = await mkdtemp(join(tmpdir(), "hya-e2e-click-"))
    await api(backend, "POST", "/v1/projects", { name: "second", roots: [secondRoot] })
    await term.waitForText(/(second|seco…) \(0\)/)
    const row = (await term.find("seco"))!
    // A full rule sits next to the Project row inside the pane, like the Sessions list's separators (list order is newest-first).
    const lines = await term.lines()
    const edge = lines[0]!.indexOf("┐")
    const rule = (line: string | undefined) => line !== undefined && /^─+$/.test(line.slice(2, edge).trimEnd())
    expect([lines[row.row - 1], lines[row.row + 1]].some(rule)).toBe(true)
    await term.attach(testInfo, "sidebar-separated")

    // A click on the row switches (Enter and the Project view do the same).
    const point = await cellPoint(term, row.row, row.col + 2)
    await term.mouse.click(point.x, point.y)
    await expectStatus(term, "Directory", secondRoot)
    await term.attach(testInfo, "sidebar-clicked")
  })

  test("about 80 columns hides it even when the terminal is otherwise wide enough for the right sidebar", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: narrow })
    await term.waitForText("Message, !shell, or @file · / commands")
    const { cols } = await term.size()
    expect(cols).toBeLessThanOrEqual(84)
    expect(await term.find("Projects")).toBeNull()
    await term.attach(testInfo, "sidebar-narrow-hidden")
    await statusSessionId(term)
    // Commands pin Projects open and select it even here.
    await term.type("/projects-sidebar on")
    await term.press("Enter")
    await term.type("/layout focus pane-2")
    await term.press("Enter")
    // The narrow split clips the title; its active border and selected row remain visible.
    await expect.poll(() => term.cell(0, 0)).toMatchObject({ char: "┌", fg: "#73c8e8" })
    await term.waitForText("▸")
    await term.attach(testInfo, "sidebar-narrow-pinned")
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
  })
})

test.describe("Project view", () => {
  test.use({ model: { steps: [textStep("ok")] } })

  test("/project lists, creates with two roots (visible in the sidebar live), edits roots, and refuses to delete a Project in use", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/project")
    await term.waitForText("Projects")
    await term.waitForText("Up/Down move · Enter opens/switches")

    // Create: name, then two roots (Enter on an empty root finishes).
    await term.press("n")
    await term.waitForText("New project name")
    await term.type("multi-root")
    await term.press("Enter")
    await term.waitForText("Root 1 (primary)")
    const rootA = await mkdtemp(join(tmpdir(), "hya-e2e-a-"))
    const rootB = await mkdtemp(join(tmpdir(), "hya-e2e-b-"))
    await term.type(rootA)
    await term.press("Enter")
    await term.type(rootB)
    await term.press("Enter")
    await term.press("Enter")
    await term.waitForText(/Created multi-root/)
    await term.waitForText("multi-root")
    await term.attach(testInfo, "project-view-created")

    // Edit roots: reorder so the second root becomes primary, then save.
    await term.press("e")
    await term.waitForText("first = primary")
    await term.press("Shift+ArrowDown")
    await term.press("Enter")
    await term.waitForText("Roots updated")

    // Delete refusal: switch into the Project (creating a live session), then try to delete it.
    await term.press("Enter")
    await expectStatus(term, "Directory", rootB)
    await prompt(term, "/project")
    await term.waitForText("multi-root")
    // The highlight starts on the active (just-switched-to) Project.
    await term.press("d")
    await term.waitForText("Enter confirms")
    await term.press("Enter")
    await term.waitForText(/failed_precondition|precondition|in use|has (a |)session/i, 10_000)
    await term.attach(testInfo, "project-view-delete-refused")
  })

  test("`t` starts a temporary session from the Project view; /new --temp does the same", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "/project")
    await term.waitForText("Projects")
    await term.waitForText("Up/Down move · Enter opens/switches")
    await term.press("t")
    const first = await statusSessionId(term)
    await prompt(term, "/new --temp")
    expect(await statusSessionId(term)).not.toBe(first)
  })
})

test.describe("--remote start", () => {
  test.use({ model: { steps: [textStep("remote reply")] } })

  test("opens the Project view automatically when no Project is active", async ({ backend, tui }) => {
    const term = await tui(["bun", tuiMain, "--server", backend.url, "--remote"])
    await term.waitForText("Projects")
    // A fresh remote start has no Project yet; the view's own empty state says so
    // without a conversation heading.
    await term.waitForText("No projects yet · n creates one")
  })

  test("--server-label names the remote and exposes its bridge URL in /status", async ({ backend, tui }) => {
    const label = "remote: relay.example.com/eh7ddx5bksrgcytl7bkai36se4"
    const term = await tui(["bun", tuiMain, "--server", backend.url, "--remote", "--server-label", label])
    await term.waitForText("No projects yet · n creates one")
    await esc(term, "No projects yet")
    await expectStatus(term, "Server", `${label} · via ${backend.url.replace(/\/$/, "")}`)

  })
})

test.describe("/sessions is scoped to the active Project", () => {
  test.use({ model: { steps: [textStep("a"), textStep("b")] } })

  test("the picker shows only the active Project's sessions and temporary ones, until F3 shows every Project", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Message, !shell, or @file · / commands")
    await prompt(term, "hello")
    await term.waitForText("a", 20_000)
    const firstId = await statusSessionId(term)

    // A second Project with its own session.
    const otherRoot = await mkdtemp(join(tmpdir(), "hya-e2e-other-"))
    await api(backend, "POST", "/v1/projects", { name: "other-project", roots: [otherRoot] })
    await prompt(term, "/project")
    await term.waitForText("other-project")
    await term.press("ArrowDown")
    await term.press("Enter")
    let otherId = firstId
    await expect.poll(async () => {
      otherId = await statusSessionId(term)
      return otherId
    }).not.toBe(firstId)
    await prompt(term, "hello other")
    await term.waitForText("b", 20_000)

    await prompt(term, "/sessions")
    await term.waitForText("F3 all")
    expect(await term.find(firstId)).toBeNull()
    await term.press("F3")
    await term.waitForText("Sessions · all projects")
    // The other Project's session is listed without a number: `/open <n>` counts only this Project's.
    await term.waitForText(firstId)
    const otherPosition = (await term.find(firstId))!
    // Check this picker entry's prefix; the same row can also contain a numbered sidebar entry.
    const otherPrefix = (await term.lines())[otherPosition.row]!.slice(Math.max(0, otherPosition.col - 6), otherPosition.col)
    expect(otherPrefix).not.toMatch(/\d\.\s*$/)
    await term.waitForText(`1. ${otherId}`)
    await esc(term, "Sessions · all projects")

    // `/open 1` is this Project's first session, as the sidebar numbers it — not the other Project's.
    await prompt(term, "/open 1")
    expect(await statusSessionId(term)).toBe(otherId)
    expect(await term.find(firstId)).toBeNull()
  })
})
