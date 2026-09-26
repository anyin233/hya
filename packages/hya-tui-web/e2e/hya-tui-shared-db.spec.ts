// One writer per database (ADR-0022, ADR-0023; docs/tui.md "Start it",
// docs/cli.md "Bare `hya`"): every TUI on a database uses its one backend
// daemon (found through `<db>.server.json`), so all see the same sessions
// and each other's live events. Bare `hya` uses the same daemon, and quitting
// any frontend leaves the daemon running.

import { spawn, type ChildProcess } from "node:child_process"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import type { Page } from "@playwright/test"
import { Tui } from "./harness"
import { expect, hyaBin, launchTest as test, selfLaunch, textStep, type Workspace } from "./hya"

const hostMain = join(dirname(fileURLToPath(import.meta.url)), "../src/main.ts")

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

const alive = (pid: number): boolean => {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

/** `/status`'s Server URL and the whole screen (the Backend row is matched by the caller). */
async function status(term: Tui): Promise<{ server: string; text: string }> {
  await prompt(term, "/status")
  await term.waitForText(/Backend\s+daemon · pid \d+/)
  const text = await term.text()
  return { server: /Server\s+(http:\/\/127\.0\.0\.1:\d+)/.exec(text)![1]!, text }
}

/** The daemon's pid, from `/status`. */
const startedPid = (text: string): number => Number(/Backend\s+daemon · pid (\d+)/.exec(text)![1])

/**
 * A second web host in a second tab, so two TUIs run at once (the `tui`
 * fixture drives one tab, and navigating it away would hang up its TUI).
 */
async function secondTab(page: Page, command: string[], cwd: string, env: Record<string, string>): Promise<{ term: Tui; host: ChildProcess }> {
  const host = spawn("bun", [hostMain, "--port", "0", "--cwd", cwd, "--", ...command], { env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] })
  const url = await new Promise<string>((resolve, reject) => {
    let output = ""
    const onData = (chunk: Buffer) => {
      output += chunk.toString()
      const match = /hya-tui-web listening on (\S+)/.exec(output)
      if (match) resolve(match[1]!)
    }
    host.stdout!.on("data", onData)
    host.stderr!.on("data", onData)
    host.once("exit", (code) => reject(new Error(`second web host exited (${code}): ${output}`)))
  })
  const tab = await page.context().newPage()
  await tab.goto(url)
  await expect.poll(() => tab.evaluate(() => window.hyaTerm?.connected ?? false)).toBe(true)
  return { term: new Tui(tab, url), host }
}

async function stopHost(host: ChildProcess): Promise<void> {
  if (host.exitCode !== null || host.signalCode !== null) return
  const exited = new Promise((resolve) => host.once("exit", resolve))
  host.kill("SIGTERM")
  await exited
}

function bareHyaEnv(workspace: Workspace): Record<string, string> {
  const { HYA_TUI_DIR: _tui, HYA_TUI_WEB_DIR: _web, ...env } = workspace.env
  return env
}

test.describe("two frontends, one database", () => {
  test.use({ model: { steps: [textStep("Reply seen by both TUIs."), textStep("Spare."), textStep("Spare.")] } })

  test("a second TUI uses the first one's daemon; both follow the same session live", async ({ tui, workspace, page }, testInfo) => {
    const first = await tui(...selfLaunch(workspace))
    await first.waitForText("Connected to hya", 30_000)
    const one = await status(first)
    const pid = startedPid(one.text)

    const [command] = selfLaunch(workspace)
    const { term: second, host } = await secondTab(page, command, workspace.dir, workspace.env)
    try {
      await second.waitForText("Connected to hya", 30_000)
      const two = await status(second)
      // Same server, not a second writer.
      expect(two.server).toBe(one.server)
      expect(two.text).toMatch(new RegExp(`Backend\\s+daemon · pid ${pid} · db /`))
      await second.attach(testInfo, "second-status")

      // The first TUI's session (created on connect); the second opens it by id.
      await first.waitForText(/hya · hysec_\w+/)
      const session = /hya · (hysec_\w+)/.exec(await first.text())![1]!
      await prompt(second, `/open ${session}`)
      await second.waitForText(new RegExp(`hya · ${session}`))

      // Live across the two TUIs: a rename and a turn in the first show up in the second.
      await prompt(first, "/rename Shared across TUIs")
      await second.waitForText(/hya · Shared across TUIs/, 20_000)
      await prompt(first, "hello from the first TUI")
      await second.waitForText("hello from the first TUI", 20_000)
      await second.waitForText("Reply seen by both TUIs.", 20_000)
      await second.attach(testInfo, "second-live")

      // Quitting either TUI leaves the daemon running.
      await prompt(second, "/exit")
      await expect.poll(() => second.page.evaluate(() => window.hyaTerm.exitCode), { timeout: 15_000 }).toBe(0)
      expect(alive(pid)).toBe(true)
    } finally {
      await stopHost(host)
    }
    await prompt(first, "/exit")
    expect(await first.waitForExit()).toBe(0)
    expect(alive(pid)).toBe(true)
  })

  test("bare hya uses the running daemon of its database and leaves it running on quit", async ({ tui, workspace, page }, testInfo) => {
    const owner = await tui(...selfLaunch(workspace))
    await owner.waitForText("Connected to hya", 30_000)
    const one = await status(owner)
    const pid = startedPid(one.text)

    const { term: bare, host } = await secondTab(page, [hyaBin, "--port", "0"], workspace.dir, bareHyaEnv(workspace))
    try {
      await bare.waitForText("Connected to hya", 60_000)
      const two = await status(bare)
      expect(two.server).toBe(one.server)
      expect(two.text).toMatch(new RegExp(`Backend\\s+daemon · pid ${pid} · db /`))
      // Bare hya still serves its WebUI next to the attached TUI.
      await bare.waitForText(/WebUI\s+http:\/\/127\.0\.0\.1:\d+/)
      await bare.attach(testInfo, "bare-attached-status")
      await prompt(bare, "/exit")
      await expect.poll(() => bare.page.evaluate(() => window.hyaTerm.exitCode), { timeout: 30_000 }).toBe(0)
    } finally {
      await stopHost(host)
    }
    // Only bare hya's frontends stopped: the daemon still answers.
    expect(alive(pid)).toBe(true)
    const health = await fetch(`${one.server}/v1/health`).then((response) => response.json() as Promise<{ ok: boolean }>)
    expect(health.ok).toBe(true)
    await prompt(owner, "/exit")
    expect(await owner.waitForExit()).toBe(0)
    expect(alive(pid)).toBe(true)
  })
})
