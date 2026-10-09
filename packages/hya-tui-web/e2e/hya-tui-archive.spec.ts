// Archive on graceful exit, `/to-background` + Ctrl+D, `--resume` / `/resume`,
// and the `/sessions` archived toggle (docs/tui.md "Sessions on start and
// exit"; ADR-0023 amendment). A graceful exit (`/exit`, Ctrl+C twice)
// archives the open session; Ctrl+D and `/to-background` quit and leave it
// running on the daemon; a WebUI tab (bare `hya` marks its tab command
// `--web-tab`) offers neither, and closing the tab leaves the session
// running too. The terminal TUI and WebUI tabs resume each other's sessions.

import { execFileSync } from "node:child_process"
import { createServer } from "node:net"
import { Tui, terminalDriver } from "./harness"
import { expectStatus, api, daemonStatus, expect, fakeModelRef, hangStep, hyaBin, launchTest as test, selfLaunch, statusSessionId, textStep, tuiInstances, type Backend, type Workspace } from "./hya"

type Session = { id: string; title?: string; archived?: boolean; busy?: boolean }

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

/** The workspace daemon as an API target. */
async function backendOf(workspace: Workspace): Promise<Backend> {
  return { url: (await daemonStatus(workspace))!.url, dir: workspace.dir }
}

async function session(backend: Backend, id: string): Promise<Session> {
  return api<Session>(backend, "GET", `/v1/sessions/${id}`)
}

async function listed(backend: Backend, query = ""): Promise<string[]> {
  const { sessions } = await api<{ sessions?: Session[] }>(backend, "GET", `/v1/sessions${query}`)
  return (sessions ?? []).map((row) => row.id)
}

/** A session made by another client (not this TUI's to drop), with a title. */
async function otherSession(backend: Backend, title: string): Promise<string> {
  const { session: created } = await api<{ session: { id: string } }>(backend, "POST", "/v1/sessions", { agent: "hya-main", model: fakeModelRef, workdir: backend.dir })
  await api(backend, "PATCH", `/v1/sessions/${created.id}`, { title })
  return created.id
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer()
    server.once("error", reject)
    server.listen(0, "127.0.0.1", () => {
      const address = server.address()
      const port = typeof address === "object" && address ? address.port : 0
      server.close(() => resolve(port))
    })
  })
}

/** `tui()` arguments that run bare `hya --port <port>` in the workspace. */
function bareHya(workspace: Workspace, port: number): [string[], { cwd: string; env: Record<string, string> }] {
  const { HYA_TUI_DIR: _tui, HYA_TUI_WEB_DIR: _web, ...env } = workspace.env
  return [[hyaBin, "--port", String(port)], { cwd: workspace.dir, env }]
}

/** Pids of the WebUI tab TUIs of `workspace` (`--web-tab`; not the host, whose argv names the command too). */
function tabTuis(workspace: Workspace): number[] {
  return execFileSync("ps", ["-axo", "pid=,command="]).toString().split("\n")
    .filter((line) => line.includes("--web-tab") && line.includes(workspace.dir) && !line.includes("--cwd"))
    .map((line) => Number(line.trim().split(/\s+/)[0]))
}

test.describe("archive on exit and resume", () => {
  test.use({ model: { steps: [textStep("First reply."), textStep("Spare."), textStep("Spare."), textStep("Spare.")] } })

  test("/exit archives the session; --continue skips it; --resume <id> and the --resume picker unarchive", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    const id = await statusSessionId(term)
    const backend = await backendOf(workspace)
    // An earlier session of the directory (another client's), for --continue to fall back to.
    const older = await otherSession(backend, "Older work")
    await prompt(term, "work to archive")
    await term.waitForText("First reply.", 20_000)
    await term.waitForIdle()

    await prompt(term, "/exit")
    expect(await term.waitForExit(20_000)).toBe(0)
    expect((await session(backend, id)).archived).toBe(true)
    expect(await listed(backend)).not.toContain(id)
    expect(await listed(backend, "?includeArchived=true")).toContain(id)

    // --continue: the most recent session that is not archived.
    const next = await tui(...selfLaunch(workspace, ["--continue"]))
    await expectStatus(next, "Session", "Older work")
    expect(await next.text()).not.toContain("First reply.")

    // --resume <id>: opened and unarchived.
    const resumed = await tui(...selfLaunch(workspace, ["--resume", id]))
    await resumed.waitForText("First reply.", 30_000)
    expect((await session(backend, id)).archived ?? false).toBe(false)
    await resumed.attach(testInfo, "resumed")

    // --resume: a picker of the directory's sessions, archived ones marked.
    await api(backend, "PATCH", `/v1/sessions/${older}`, { archived: true })
    const picking = await tui(...selfLaunch(workspace, ["--resume"], { viewport: { width: 690, height: 640 } }))
    await picking.waitForText("Resume a session · archived ones included", 30_000)
    await picking.waitForText("Older work")
    await picking.waitForText("archived")
    await picking.attach(testInfo, "resume-picker")
    await picking.type("Older")
    await picking.press("Enter")
    await expectStatus(picking, "Session", "Older work")
    await expect.poll(async () => (await session(backend, older)).archived ?? false).toBe(false)
  })
})

test.describe("background exits keep the session running", () => {
  test.use({ model: { steps: [hangStep(60_000), textStep("Spare."), textStep("Spare."), textStep("Spare.")] } })

  for (const way of ["Ctrl+D", "/to-background"] as const) {
    test(`${way} quits at once without archiving; the turn finishes on the daemon`, async ({ tui, workspace, fakeModel }) => {
      const term = await tui(...selfLaunch(workspace))
      await term.waitForText("Message, !shell, or @file · / commands", 30_000)
      const id = await statusSessionId(term)
      const backend = await backendOf(workspace)
      await prompt(term, "a long job")
      await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
      if (way === "Ctrl+D") await term.press("Control+d")
      else await prompt(term, "/to-background")
      expect(await term.waitForExit(20_000)).toBe(0)
      // Still running on the daemon, not archived.
      const running = await session(backend, id)
      expect(running.busy).toBe(true)
      expect(running.archived ?? false).toBe(false)
      fakeModel!.release()
      await expect.poll(async () => (await session(backend, id)).busy ?? false, { timeout: 20_000 }).toBe(false)
      expect((await session(backend, id)).archived ?? false).toBe(false)
      expect(await listed(backend)).toContain(id)
    })
  }
})

test.describe("/sessions archived toggle", () => {
  test("/sessions shows archived chats by default; Ctrl+A hides them and opening one unarchives it", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace, [], { viewport: { width: 690, height: 640 } }))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    const backend = await backendOf(workspace)
    const hidden = await otherSession(backend, "Shelved work")
    await api(backend, "PATCH", `/v1/sessions/${hidden}`, { archived: true })

    await prompt(term, "/sessions")
    await term.waitForText("Sessions · archived included")
    await term.waitForText("Shelved work")
    await term.waitForText("Ctrl+A hides archived")
    await term.press("Control+a")
    await term.waitForText("Ctrl+A shows archived")
    expect(await term.text()).not.toContain("Shelved work")
    await term.press("Control+a")
    await term.waitForText("Shelved work")
    await term.attach(testInfo, "archived-shown")
    await term.type("Shelved")
    await term.press("Enter")
    await expectStatus(term, "Session", "Shelved work")
    await expect.poll(async () => (await session(backend, hidden)).archived ?? false).toBe(false)

    // Another client archives the open session: the sidebar marks it live
    // (the narrow Sessions box cuts the row to `hya-main · arc…`).
    await term.resize(1800, 640)
    await term.waitForText("─Sessions")
    await api(backend, "PATCH", `/v1/sessions/${hidden}`, { archived: true })
    await term.waitForText(/hya-main · arc/, 20_000)
  })
})

test.describe("WebUI tabs (bare hya)", { tag: "@browser-only" }, () => {
  test.skip(terminalDriver === "pty", "Browser tab disconnect and WebUI-only commands require Chromium")
  test.use({ model: { steps: [hangStep(60_000), textStep("Spare."), textStep("Spare."), textStep("Spare.")] } })

  test("a tab offers no /to-background, Ctrl+D only shows a notice, and closing the tab leaves the session unarchived and running", async ({ tui, workspace, page, fakeModel }, testInfo) => {
    const port = await freePort()
    const term = await tui(...bareHya(workspace, port))
    await term.waitForText("Message, !shell, or @file · / commands", 60_000)
    const webPage = await term.page.context().newPage()
    await webPage.setViewportSize({ width: 690, height: 640 })
    await webPage.goto(`http://127.0.0.1:${port}/`)
    const web = new Tui(webPage, `http://127.0.0.1:${port}/`)
    await web.waitForText("Message, !shell, or @file · / commands", 30_000)
    const id = await statusSessionId(web)
    const backend = await backendOf(workspace)

    await prompt(web, "/to-background")
    expect(tuiInstances(tabTuis(workspace)).length).toBe(1)
    await web.press("Control+d")
    expect(tuiInstances(tabTuis(workspace)).length).toBe(1)
    // The command pane offers /todos but not /to-background in the tab…
    await web.type("/to")
    await web.waitForText("/todos")
    expect(await web.text()).not.toContain("/to-background")
    await web.attach(testInfo, "web-menu")
    await web.press("Escape")
    // …and does offer it in the terminal.
    await term.type("/to")
    await term.waitForText("/to-background")
    await term.press("Escape")

    // A turn runs in the tab; closing the tab (SIGHUP to its TUI) archives nothing.
    await prompt(web, "a long web job")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    expect(tuiInstances(tabTuis(workspace)).length).toBe(1)
    await webPage.close()
    await expect.poll(() => tabTuis(workspace).length, { timeout: 20_000 }).toBe(0)
    const left = await session(backend, id)
    expect(left.archived ?? false).toBe(false)
    expect(left.busy).toBe(true)
    fakeModel!.release()
    await expect.poll(async () => (await session(backend, id)).busy ?? false, { timeout: 20_000 }).toBe(false)
    expect((await session(backend, id)).archived ?? false).toBe(false)
  })
})

test.describe("resume across the terminal and WebUI tabs (bare hya)", () => {
  test.use({ model: { steps: [textStep("Terminal reply."), textStep("Web reply."), textStep("Spare."), textStep("Spare.")] } })

  test("/resume in a tab opens the terminal's session, and the other way round", async ({ tui, workspace }, testInfo) => {
    const port = await freePort()
    const term = await tui(...bareHya(workspace, port))
    await term.waitForText("Message, !shell, or @file · / commands", 60_000)
    const terminalId = await statusSessionId(term)
    await prompt(term, "terminal work")
    await term.waitForText("Terminal reply.", 20_000)
    await term.waitForIdle()

    const web = terminalDriver === "pty"
      ? await tui(...selfLaunch(workspace, [], { independent: true }))
      : await (async () => {
          const webPage = await term.page.context().newPage()
          await webPage.goto(`http://127.0.0.1:${port}/`)
          return new Tui(webPage, `http://127.0.0.1:${port}/`)
        })()
    // Plain launches restore the saved Project chat. Start a fresh chat explicitly
    // before testing cross-client resume in both directions.
    await web.waitForText("Message, !shell, or @file · / commands", 30_000)
    expect(await statusSessionId(web)).toBe(terminalId)
    await web.waitForText("Terminal reply.")
    await prompt(web, "/new")
    const webId = await statusSessionId(web)
    expect(webId).not.toBe(terminalId)
    expect(await web.text()).not.toContain("Terminal reply.")
    await prompt(web, "web work")
    await web.waitForText("Web reply.", 20_000)
    await web.waitForIdle()

    await prompt(web, "/resume")
    await web.waitForText("Resume a session · archived ones included")
    await web.type(terminalId)
    await web.press("Enter")
    await web.waitForText("Terminal reply.", 20_000)
    await web.attach(testInfo, "web-resumed")

    await prompt(term, "/resume")
    await term.waitForText("Resume a session · archived ones included")
    await term.type(webId)
    await term.press("Enter")
    await term.waitForText("Web reply.", 20_000)
    await term.resize(690, 640)
    expect(await statusSessionId(term)).toBe(webId)
    await term.attach(testInfo, "terminal-resumed")
  })
})
