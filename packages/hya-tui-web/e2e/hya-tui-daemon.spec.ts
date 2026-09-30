// The backend daemon (ADR-0023; docs/tui.md "When the server goes away"):
// the server outlives its TUIs, and `hya serve stop` / `restart` control it.
// The server says why it goes away (`serverStopping {reason}`): after `stop`
// a TUI starts nothing and waits for `/reconnect`; after `restart` it waits
// for the next daemon and attaches. Only an unexpected loss (a crash, kill
// -9) makes it find or start the next server by itself; with two TUIs, the
// database lock makes exactly one of them start it, the other attaches.

import type { Page } from "@playwright/test"
import { execFileSync, spawn, type ChildProcess } from "node:child_process"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { Tui } from "./harness"
import { daemon, daemonStatus, expect, launchTest as test, selfLaunch, textStep, workspaceDb, type Workspace } from "./hya"

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

/** The untitled open session's id from `/status`. */
async function statusSessionId(term: Tui): Promise<string> {
  await prompt(term, "/status")
  await term.waitForText(/Session\s+hysec_\w+/)
  return /Session\s+(hysec_\w+)/.exec(await term.text())![1]!
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

/** Pids of every `hya serve` process of the workspace database. */
function servePids(workspace: Workspace): number[] {
  let out = ""
  try {
    // A successor inherits the listener and may omit the original --bind argv;
    // match the database-bearing serve command rather than one exact argv.
    const db = workspaceDb(workspace).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    out = execFileSync("pgrep", ["-f", `serve .*--db ${db}`]).toString()
  } catch {
    // pgrep exits 1 when nothing matches.
  }
  return out.split("\n").filter(Boolean).map(Number)
}

async function stopHost(host: ChildProcess): Promise<void> {
  if (host.exitCode !== null || host.signalCode !== null) return
  const exited = new Promise((resolve) => host.once("exit", resolve))
  host.kill("SIGTERM")
  await exited
}

test.describe("backend daemon", () => {
  test.use({ model: { steps: [textStep("Before the stop."), textStep("After the new server."), textStep("Spare."), textStep("Spare.")] } })

  test("`hya serve stop` under a TUI: nothing starts a new daemon until /reconnect, which then works", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    const session = await statusSessionId(term)
    const before = await statusPid(term)
    await prompt(term, "first prompt")
    await term.waitForText("Before the stop.", 20_000)
    await term.waitForIdle()

    const stopped = await daemon(workspace, ["stop"])
    expect(stopped.code).toBe(0)
    expect(stopped.stdout).toContain(`stopped hya server pid ${before}`)
    expect(stopped.stdout).toContain("connected TUIs stay disconnected until /reconnect")

    // The TUI was told it was a manual stop and starts nothing.
    await expect.poll(() => daemonStatus(workspace), { timeout: 20_000 }).toBeUndefined()
    await term.attach(testInfo, "stopped")
    // Prompts are refused while stopped (and the stream keeps retrying meanwhile).
    await prompt(term, "lost prompt")
    expect(await daemonStatus(workspace)).toBeUndefined()
    expect(servePids(workspace)).toEqual([])

    // /reconnect starts the next daemon, reloads the session, and it works.
    await prompt(term, "/reconnect")
    let after = before
    await expect.poll(async () => {
      after = (await daemonStatus(workspace))?.pid ?? before
      return after
    }, { timeout: 30_000 }).not.toBe(before)
    expect(after).not.toBe(before)
    expect((await daemonStatus(workspace))?.pid).toBe(after)
    await term.waitForText("Before the stop.")
    await prompt(term, "second prompt")
    await term.waitForText("After the new server.", 20_000)
    await term.waitForIdle()
    expect(await statusPid(term)).toBe(after)
  })

  test("`hya serve restart` under a TUI: it waits for the new daemon and attaches; still exactly one daemon", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    await prompt(term, "first prompt")
    await term.waitForText("Before the stop.", 20_000)
    await term.waitForIdle()
    const initial = await daemonStatus(workspace)
    expect(initial?.pid).toBeDefined()
    const before = initial!.pid
    // Also exercise reconnect behavior at about 80 columns.
    await term.resize(690, 640)

    const restarted = await daemon(workspace, ["restart", "--json"])
    expect(restarted.code).toBe(0)
    const next = JSON.parse(restarted.stdout.trim()) as { pid: number; queued: boolean }
    expect(next.queued).toBe(true)
    expect(next.pid).toBe(before)
    // Restart reports the queued request against the old owner. Poll until
    // handoff publishes the actual successor generation.
    let successorPid = before
    await expect.poll(async () => {
      successorPid = (await daemonStatus(workspace))?.pid ?? before
      return successorPid !== before
    }, { timeout: 30_000 }).toBe(true)
    expect(successorPid).not.toBe(before)
    await expect.poll(() => servePids(workspace)).toEqual([successorPid])
    await prompt(term, "after the restart")
    await term.waitForText("After the new server.", 20_000)
    expect(await statusPid(term)).toBe(successorPid)
    await expect.poll(() => statusPid(term), { timeout: 30_000 }).toBe(successorPid)
    await expect.poll(() => servePids(workspace)).toEqual([successorPid])
  })

  test("a daemon killed with SIGKILL (no reason sent): the TUI starts the next one by itself", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    const before = await statusPid(term)
    process.kill(before, "SIGKILL")
    let after = before
    await expect.poll(async () => {
      after = (await daemonStatus(workspace))?.pid ?? before
      return after
    }, { timeout: 30_000 }).not.toBe(before)
    expect(after).not.toBe(before)
    expect((await daemonStatus(workspace))?.pid).toBe(after)
    await term.attach(testInfo, "after-kill")
    await prompt(term, "after the crash")
    await term.waitForText("Before the stop.", 20_000)
  })

  test("two TUIs lose the daemon to a crash: exactly one starts the next, the other attaches to it", async ({ tui, workspace, page }, testInfo) => {
    const first = await tui(...selfLaunch(workspace))
    await first.waitForText("Message, !shell, or @file · / commands", 30_000)
    const { term: second, host } = await secondTab(page, workspace)
    try {
      await second.waitForText("Message, !shell, or @file · / commands", 30_000)
      const session = await statusSessionId(first)
      const before = await statusPid(first)
      expect(await statusPid(second)).toBe(before)

      process.kill(before, "SIGKILL")
      let pid = before
      await expect.poll(async () => {
        pid = (await daemonStatus(workspace))?.pid ?? before
        return pid
      }, { timeout: 30_000 }).not.toBe(before)
      expect(pid).not.toBe(before)
      await expect.poll(() => statusPid(first), { timeout: 30_000 }).toBe(pid)
      await expect.poll(() => statusPid(second), { timeout: 30_000 }).toBe(pid)
      await second.attach(testInfo, "second-after")

      // Both still share live state: the second follows the first's session.
      await prompt(first, "shared after the move")
      await prompt(second, `/open ${session}`)
      await second.waitForText("shared after the move", 20_000)
    } finally {
      await stopHost(host)
    }
  })
})
