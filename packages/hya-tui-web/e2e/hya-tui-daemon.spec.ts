// The backend daemon (ADR-0023; docs/tui.md "When the server goes away"):
// the server outlives its TUIs, and `hya serve stop` / `restart` control it.
// A TUI that knows its database reconnects when its server goes away: it
// finds the next server through the discovery file or starts one, switches
// to it, and reloads the open session. With two TUIs, the database lock makes
// exactly one of them start the new daemon; the other attaches to it.

import type { Page } from "@playwright/test"
import { spawn, type ChildProcess } from "node:child_process"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { Tui } from "./harness"
import { daemon, daemonStatus, expect, launchTest as test, selfLaunch, textStep, type Workspace } from "./hya"

const hostMain = join(dirname(fileURLToPath(import.meta.url)), "../src/main.ts")

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

/** The daemon pid `/status` names. */
async function statusPid(term: Tui): Promise<number> {
  await prompt(term, "/status")
  await term.waitForText(/Backend\s+daemon · pid \d+/)
  return Number(/Backend\s+daemon · pid (\d+)/.exec(await term.text())![1])
}

/** A second web host in a second tab, so two TUIs run at once. */
async function secondTab(page: Page, workspace: Workspace): Promise<{ term: Tui; host: ChildProcess }> {
  const [command] = selfLaunch(workspace)
  const host = spawn("bun", [hostMain, "--port", "0", "--cwd", workspace.dir, "--", ...command], { env: { ...process.env, ...workspace.env }, stdio: ["ignore", "pipe", "pipe"] })
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

test.describe("backend daemon", () => {
  test.use({ model: { steps: [textStep("Before the stop."), textStep("After the new server."), textStep("Spare."), textStep("Spare.")] } })

  test("`hya serve stop` under a TUI: it starts a new daemon, reloads the session, and keeps working", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Connected to hya", 30_000)
    await prompt(term, "first prompt")
    await term.waitForText("Before the stop.", 20_000)
    await term.waitForText(/^Ready/m)
    const session = /hya · (\S+) · build/.exec(await term.text())![1]!
    const before = await statusPid(term)

    const stopped = await daemon(workspace, ["stop"])
    expect(stopped.code).toBe(0)
    expect(stopped.stdout).toContain(`stopped hya server pid ${before}`)

    // The TUI noticed, started the next daemon, and says so.
    await term.waitForText(/Started a new server · pid \d+/, 30_000)
    const after = Number(/Started a new server · pid (\d+)/.exec(await term.text())![1])
    expect(after).not.toBe(before)
    expect((await daemonStatus(workspace))?.pid).toBe(after)
    // Same session, transcript reloaded from the database.
    await term.waitForText(new RegExp(`hya · ${session}`))
    await term.waitForText("Before the stop.")
    await term.attach(testInfo, "after-restart")

    // It keeps working on the new server, and /status names it.
    await prompt(term, "second prompt")
    await term.waitForText("After the new server.", 20_000)
    await term.waitForText(/^Ready/m)
    expect(await statusPid(term)).toBe(after)

    // About 80 columns: the notice still reads.
    await term.resize(690, 640)
    const restarted = await daemon(workspace, ["restart"])
    expect(restarted.code).toBe(0)
    await term.waitForText(/Server moved · now pid \d+/, 30_000)
    await term.attach(testInfo, "narrow-moved")
  })

  test("two TUIs lose the daemon: exactly one starts the next, the other attaches to it", async ({ tui, workspace, page }, testInfo) => {
    const first = await tui(...selfLaunch(workspace))
    await first.waitForText("Connected to hya", 30_000)
    const { term: second, host } = await secondTab(page, workspace)
    try {
      await second.waitForText("Connected to hya", 30_000)
      const before = await statusPid(first)
      expect(await statusPid(second)).toBe(before)

      expect((await daemon(workspace, ["stop"])).code).toBe(0)
      const notice = /Started a new server · pid \d+|Server moved · now pid \d+/
      await first.waitForText(notice, 30_000)
      await second.waitForText(notice, 30_000)
      const texts = [await first.text(), await second.text()]
      const started = texts.filter((text) => /Started a new server · pid \d+/.test(text))
      const moved = texts.filter((text) => /Server moved · now pid \d+/.test(text))
      expect(started).toHaveLength(1)
      expect(moved).toHaveLength(1)
      const pid = (await daemonStatus(workspace))!.pid
      expect(pid).not.toBe(before)
      expect(await statusPid(first)).toBe(pid)
      expect(await statusPid(second)).toBe(pid)
      await second.attach(testInfo, "second-after")

      // Both still share live state: the second follows the first's session.
      await prompt(first, "shared after the move")
      const session = /hya · (\S+) · build/.exec(await first.text())![1]!
      await prompt(second, `/open ${session}`)
      await second.waitForText("shared after the move", 20_000)
    } finally {
      await stopHost(host)
    }
  })
})
