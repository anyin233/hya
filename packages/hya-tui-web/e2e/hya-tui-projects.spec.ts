// The left Projects sidebar and the full-screen Project view (docs/tui.md
// "Projects"): the sidebar's live list with a busy marker and switching, the
// view's create/edit-roots/rename/delete flows, `--remote` opening the view
// automatically, and the `/sessions` picker's project scoping and
// "all projects" toggle.

import { mkdtemp } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { Tui } from "./harness"
import { api, expect, hyaTui, test, textStep, tuiMain } from "./hya"

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

test.describe("Projects sidebar", () => {
  test.use({ model: { steps: [textStep("first reply"), textStep("second reply")] } })

  test("hidden at the default viewport (both sidebars would not fit next to the chat column)", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    expect(await term.find("Projects")).toBeNull()
  })

  test("a wide terminal shows every Project, the active one and a session count; Enter switches", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Connected to hya")
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
    await term.press("n")
    await term.type("second")
    await term.press("Enter")
    const secondRoot = await mkdtemp(join(tmpdir(), "hya-e2e-second-"))
    await term.type(secondRoot)
    await term.press("Enter")
    await term.press("Enter")
    await term.waitForText(/Created second/)
    await esc(term, "Created second")

    await term.waitForText("second")
    await term.press("Control+p")
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText(/Project second/, 20_000)
  })

  test("about 80 columns hides it even when the terminal is otherwise wide enough for the right sidebar", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: narrow })
    await term.waitForText("Connected to hya")
    const { cols } = await term.size()
    expect(cols).toBeLessThanOrEqual(84)
    expect(await term.find("Projects")).toBeNull()
    await term.attach(testInfo, "sidebar-narrow-hidden")
    // Ctrl+P opens (and focuses) it even here.
    await term.press("Control+p")
    await term.waitForText("Projects")
    await term.attach(testInfo, "sidebar-narrow-pinned")
    for (const line of await term.lines()) expect(line.length).toBeLessThanOrEqual(cols)
  })
})

test.describe("Project view", () => {
  test.use({ model: { steps: [textStep("ok")] } })

  test("/project lists, creates with two roots (visible in the sidebar live), edits roots, and refuses to delete a Project in use", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Connected to hya")
    await prompt(term, "/project")
    await term.waitForText("Projects")
    await term.waitForText("Up/Down move · Enter opens/switches")

    // Create: name, then two roots (Enter on an empty root finishes).
    await term.press("n")
    await term.type("multi-root")
    await term.press("Enter")
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
    await term.waitForText(/Project multi-root/)
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
    await term.waitForText("Connected to hya")
    await prompt(term, "/project")
    await term.waitForText("Projects")
    await term.press("t")
    await term.waitForText(/hya · hysec_\w+/, 20_000)
    await prompt(term, "/new --temp")
    await term.waitForText(/hya · hysec_\w+/, 20_000)
  })
})

test.describe("--remote start", () => {
  test.use({ model: { steps: [textStep("remote reply")] } })

  test("opens the Project view automatically when no Project is active", async ({ backend, tui }) => {
    const term = await tui(["bun", tuiMain, "--server", backend.url, "--remote"])
    await term.waitForText("Projects")
    // A fresh remote start has no Project yet; the view's own empty state says so
    // (the status line's `noProjectStatus` sits underneath the full-screen view).
    await term.waitForText("No projects yet · n creates one")
  })
})

test.describe("/sessions is scoped to the active Project", () => {
  test.use({ model: { steps: [textStep("a"), textStep("b")] } })

  test("the picker shows only the active Project's sessions and temporary ones, until F3 shows every Project", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wide })
    await term.waitForText("Connected to hya")
    await prompt(term, "hello")
    await term.waitForText("a", 20_000)
    await term.waitForText(/hya · hysec_\w+/)
    const firstId = /hya · (hysec_\w+)/.exec(await term.text())![1]!

    // A second Project with its own session.
    const otherRoot = await mkdtemp(join(tmpdir(), "hya-e2e-other-"))
    await api(backend, "POST", "/v1/projects", { name: "other-project", roots: [otherRoot] })
    await prompt(term, "/project")
    await term.waitForText("other-project")
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.waitForText("b", 20_000)

    await prompt(term, "/sessions")
    await term.waitForText("F3 all")
    expect(await term.find(firstId)).toBeNull()
    await term.press("F3")
    await term.waitForText("Sessions · all projects")
    await term.waitForText(firstId)
  })
})
