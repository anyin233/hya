// Bare `hya` (docs/cli.md "Bare `hya`", ADR-0020, ADR-0023): on a terminal it
// uses the backend daemon of its database (starting one if none runs), then
// runs the WebUI on `--port` and the terminal TUI against it; both frontends
// share that daemon and its sessions, and quitting leaves the daemon running.
// `--backend <url>` names the server instead. The host runs
// `target/debug/hya` itself on a real PTY, so this is exactly what a user's
// terminal gets. The packages are found through the workspace
// (`HYA_TUI_DIR` / `HYA_TUI_WEB_DIR` unset).

import { execFileSync } from "node:child_process"
import { mkdir, readFile } from "node:fs/promises"
import { createServer, type Server } from "node:net"
import { join } from "node:path"
import { Tui } from "./harness"
import { daemon, daemonStatus, expect, hyaBin, launchTest as test, type Workspace } from "./hya"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

/** Listen on a free loopback port; the caller closes it (or keeps it busy). */
function listen(): Promise<{ server: Server; port: number }> {
  return new Promise((resolve, reject) => {
    const server = createServer()
    server.once("error", reject)
    server.listen(0, "127.0.0.1", () => {
      const address = server.address()
      if (typeof address === "object" && address) resolve({ server, port: address.port })
    })
  })
}

async function freePort(): Promise<number> {
  const { server, port } = await listen()
  await new Promise((resolve) => server.close(resolve))
  return port
}

/** `tui()` arguments that run bare `hya --port <port>` in the workspace with its isolated environment. */
function bareHya(workspace: Workspace, port: number, env: Record<string, string> = {}): [string[], { cwd: string; env: Record<string, string> }] {
  const { HYA_TUI_DIR: _tui, HYA_TUI_WEB_DIR: _web, ...inherited } = { ...workspace.env, ...env }
  return [[hyaBin, "--port", String(port)], { cwd: workspace.dir, env: { ...inherited, ...env } }]
}

/** The backend's URL, read from `/status` in the terminal TUI. */
async function backendUrl(term: Tui): Promise<string> {
  await prompt(term, "/status")
  await term.waitForText(/Server\s+http:\/\/127\.0\.0\.1:\d+/)
  return /Server\s+(http:\/\/127\.0\.0\.1:\d+)/.exec(await term.text())![1]!
}

async function reachable(url: string): Promise<boolean> {
  try {
    await fetch(url, { signal: AbortSignal.timeout(2_000) })
    return true
  } catch {
    return false
  }
}

/**
 * Pids whose command line contains `needle` but not `except` (`ps`, not
 * `pgrep`, to match the full argv on macOS and Linux).
 */
function pids(needle: string, except?: string): number[] {
  return execFileSync("ps", ["-axo", "pid=,command="]).toString().split("\n")
    .filter((line) => line.includes(needle) && !(except && line.includes(except)))
    .map((line) => Number(line.trim().split(/\s+/)[0]))
}

test.describe("bare hya", () => {
  test("starts the terminal TUI and a WebUI on --port that share one backend and each other's sessions", async ({ tui, workspace, page }, testInfo) => {
    const port = await freePort()
    const term = await tui(...bareHya(workspace, port))
    await term.waitForText("Connected to hya", 60_000)
    await term.waitForText(`WebUI http://127.0.0.1:${port}`)
    // A session created in the terminal (titled after its first prompt)…
    await prompt(term, "hello from the terminal")
    await term.waitForText(/^Ready/m, 20_000)
    const backend = await backendUrl(term)

    // …appears in the WebUI: the same TUI, served on --port, connected to the same server.
    const webPage = await page.context().newPage()
    await webPage.goto(`http://127.0.0.1:${port}/`)
    await expect.poll(() => webPage.evaluate(() => window.hyaTerm?.connected ?? false)).toBe(true)
    const web = new Tui(webPage, `http://127.0.0.1:${port}/`)
    await web.waitForText("Connected to hya", 30_000)
    // The header names the server the tab's TUI is connected to.
    await web.waitForText(`${backend}/`)
    await prompt(web, "/sessions")
    await web.waitForText("New session")
    await web.waitForText("hello from the terminal")
    await web.attach(testInfo, "webui")
    await web.press("Escape")
    // Closed before typing (an Esc right before a key reads as Alt+key).
    await expect.poll(async () => (await web.text()).includes("Esc closes")).toBe(false)

    // …and the other way round: the tab's session shows up in the terminal.
    await prompt(web, "hello from the web tab")
    await web.waitForText(/^Ready/m, 20_000)
    // Titled after its first prompt (the automatic title arrives a moment later).
    await web.waitForText(/hya · hello from the web tab/, 20_000)
    await prompt(term, "/sessions")
    await term.waitForText("hello from the web tab")
    await term.press("Escape")
    await term.attach(testInfo, "terminal-sessions")

    // About 80 columns: the sidebar hides and the status bar still fits the WebUI address.
    await term.resize(690, 640)
    await term.waitForText(new RegExp(`^mode manual · .* · WebUI http://127\\.0\\.0\\.1:${port}\\b`, "m"))
    await term.attach(testInfo, "terminal-narrow")
  })

  test("a busy --port shows the WebUI unavailable notice while the TUI keeps working", async ({ tui, workspace }) => {
    const { server, port } = await listen()
    try {
      const term = await tui(...bareHya(workspace, port))
      await term.waitForText(`WebUI unavailable: port ${port} is in use · hya --port <N>`, 60_000)
      await term.waitForText("WebUI unavailable")
      await prompt(term, "still works")
      await term.waitForText(/^Ready/m, 20_000)
      await prompt(term, "/status")
      await term.waitForText(`WebUI       unavailable: port ${port} is in use`)
    } finally {
      server.close()
    }
  })

  test("/exit stops the WebUI host and its tabs' TUIs, leaves the daemon running, and exits 0", async ({ tui, workspace, page }) => {
    const port = await freePort()
    const term = await tui(...bareHya(workspace, port))
    await term.waitForText(`WebUI http://127.0.0.1:${port}`, 60_000)
    const backend = await backendUrl(term)
    const webPage = await page.context().newPage()
    await webPage.goto(`http://127.0.0.1:${port}/`)
    const web = new Tui(webPage, `http://127.0.0.1:${port}/`)
    await web.waitForText("Connected to hya", 30_000)

    const host = pids(`--port ${port} --cwd`)
    // The web host's own argv names the TUI command too; count only TUI processes.
    const tuis = pids(`--server ${backend}`, "--cwd")
    expect(host.length).toBe(1)
    // The terminal TUI and the WebUI tab's TUI.
    expect(tuis.length).toBe(2)
    expect(await reachable(backend)).toBe(true)

    await prompt(term, "/exit")
    expect(await term.waitForExit(20_000)).toBe(0)
    await expect.poll(() => pids(`--port ${port} --cwd`).length, { timeout: 15_000 }).toBe(0)
    await expect.poll(() => pids(`--server ${backend}`).length, { timeout: 15_000 }).toBe(0)
    expect([...host, ...tuis].filter((pid) => { try { process.kill(pid, 0); return true } catch { return false } })).toEqual([])
    await expect.poll(() => reachable(`http://127.0.0.1:${port}/`), { timeout: 15_000 }).toBe(false)
    await web.waitForText("process exited")
    // The daemon outlives its frontends.
    expect(await reachable(backend)).toBe(true)
    expect((await daemonStatus(workspace))?.url).toBe(backend)
    // Notices and web host output went to the log, not the terminal; the daemon logs next to its database.
    const log = await readFile(join(workspace.env.XDG_STATE_HOME!, "hya", "hya.log"), "utf8")
    expect(log).toMatch(new RegExp(`hya: started the backend daemon pid \\d+ at ${backend}`))
    expect(log).toContain(`[webui] hya-tui-web listening on http://127.0.0.1:${port}/`)
    expect(log).toContain("hya: frontends stopped (Ok(0))")
    const daemonLog = await readFile(join(workspace.env.XDG_STATE_HOME!, "hya", "sessions.db.server.log"), "utf8")
    expect(daemonLog).toContain(`hya server listening on ${backend}`)
  })

  for (const [signal, code] of [["SIGTERM", 143], ["SIGHUP", 129]] as const) {
    test(`${signal} to hya stops the TUI, the WebUI host, and its tabs' TUIs; the daemon keeps running`, async ({ tui, workspace, page }) => {
      const port = await freePort()
      const term = await tui(...bareHya(workspace, port))
      await term.waitForText(`WebUI http://127.0.0.1:${port}`, 60_000)
      const backend = await backendUrl(term)
      const webPage = await page.context().newPage()
      await webPage.goto(`http://127.0.0.1:${port}/`)
      await new Tui(webPage, `http://127.0.0.1:${port}/`).waitForText("Connected to hya", 30_000)
      // Not the test harness host, whose argv ends with the same command.
      const hya = pids(`${hyaBin} --port ${port}`, "--cwd")
      const children = [...pids(`--port ${port} --cwd`), ...pids(`--server ${backend}`, "--cwd")]
      expect(hya.length).toBe(1)
      expect(children.length).toBe(3)

      if (signal === "SIGTERM") {
        process.kill(hya[0]!, signal)
        expect(await term.waitForExit(20_000)).toBe(code)
      } else {
        // Closing the tab that runs hya: the host sends its process SIGHUP.
        await page.goto("about:blank")
      }
      const alive = () => [...hya, ...children].filter((pid) => { try { process.kill(pid, 0); return true } catch { return false } })
      await expect.poll(alive, { timeout: 20_000 }).toEqual([])
      await expect.poll(() => reachable(`http://127.0.0.1:${port}/`), { timeout: 15_000 }).toBe(false)
      expect(await reachable(backend)).toBe(true)
    })
  }

  test("when the daemon stops, the terminal TUI and the WebUI tab find the next one (one starts it)", async ({ tui, workspace, page }, testInfo) => {
    const port = await freePort()
    const term = await tui(...bareHya(workspace, port))
    await term.waitForText(`WebUI http://127.0.0.1:${port}`, 60_000)
    const before = (await daemonStatus(workspace))!.pid
    const webPage = await page.context().newPage()
    await webPage.goto(`http://127.0.0.1:${port}/`)
    const web = new Tui(webPage, `http://127.0.0.1:${port}/`)
    await web.waitForText("Connected to hya", 30_000)

    expect((await daemon(workspace, ["stop"])).code).toBe(0)
    const notice = /Started a new server · pid \d+|Server moved · now pid \d+/
    await term.waitForText(notice, 30_000)
    await web.waitForText(notice, 30_000)
    const texts = [await term.text(), await web.text()]
    expect(texts.filter((text) => /Started a new server/.test(text))).toHaveLength(1)
    const after = (await daemonStatus(workspace))!.pid
    expect(after).not.toBe(before)
    await prompt(term, "still works after the move")
    await term.waitForText(/^Ready/m, 20_000)
    await term.attach(testInfo, "terminal-after")
    // A new tab of the same host (its command still names the old URL) finds the new daemon too.
    const late = await page.context().newPage()
    await late.goto(`http://127.0.0.1:${port}/`)
    const lateTui = new Tui(late, `http://127.0.0.1:${port}/`)
    await lateTui.waitForText("Connected to hya", 30_000)
    await prompt(lateTui, "/status")
    await lateTui.waitForText(new RegExp(`Backend\\s+daemon · pid ${after}`))
  })

  test("--backend uses that server as is; an unreachable one is an error", async ({ tui, workspace }, testInfo) => {
    const started = await daemon(workspace, ["start", "--json"])
    expect(started.code).toBe(0)
    const { url, pid } = JSON.parse(started.stdout.trim()) as { url: string; pid: number }
    const port = await freePort()
    const { HYA_TUI_DIR: _tui, HYA_TUI_WEB_DIR: _web, ...env } = workspace.env
    const term = await tui([hyaBin, "--port", String(port), "--backend", url], { cwd: workspace.dir, env })
    await term.waitForText("Connected to hya", 60_000)
    await prompt(term, "/status")
    await term.waitForText(`Server      ${url}`)
    await term.waitForText(new RegExp(`Backend\\s+daemon · pid ${pid} · via --backend/--server`))
    await term.attach(testInfo, "explicit-status")
    await prompt(term, "/exit")
    expect(await term.waitForExit(20_000)).toBe(0)
    expect(await reachable(url)).toBe(true)

    const dead = `http://127.0.0.1:${await freePort()}`
    const refused = await tui([hyaBin, "--port", String(await freePort()), "--backend", dead], { cwd: workspace.dir, env })
    await refused.waitForText(`no hya server answers at --backend ${dead}`)
    expect(await refused.waitForExit()).toBe(1)
  })

  test("missing TUI assets or Bun are a clear error and exit status 1", async ({ tui, workspace }) => {
    const empty = join(workspace.root, "empty")
    await mkdir(empty, { recursive: true })
    const assets = await tui(...bareHya(workspace, await freePort(), { HYA_TUI_DIR: empty }))
    await assets.waitForText(`HYA_TUI_DIR=${empty} has no src/main.ts`)
    expect(await assets.waitForExit()).toBe(1)

    const bun = await tui(...bareHya(workspace, await freePort(), { BUN: join(empty, "bun") }))
    await bun.waitForText(`Bun not found at ${join(empty, "bun")}`)
    expect(await bun.waitForExit()).toBe(1)
  })
})
