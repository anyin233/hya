// The backend daemon (ADR-0023; docs/tui.md "When the server goes away"):
// the server outlives its TUIs, and `hya serve stop` / `restart` control it.
// The server says why it goes away (`serverStopping {reason}`): after `stop`
// a TUI starts nothing and waits for `/reconnect`; after `restart` it waits
// for the next daemon and attaches. Only an unexpected loss (a crash, kill
// -9) makes it find or start the next server by itself; with two TUIs, the
// database lock makes exactly one of them start it, the other attaches.

import { execFileSync } from "node:child_process"

import { Tui } from "./harness"
import { daemon, daemonStatus, expect, launchTest as test, selfLaunch, textStep, tuiInstances, tuiMain, workspaceDb, type Workspace } from "./hya"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

/** Read one status snapshot, reopening the view if reconnect restores chat. */
async function statusField(term: Tui, pattern: RegExp, expected?: string): Promise<string> {
  let value: string | undefined
  await expect.poll(async () => {
    await prompt(term, "/status")
    // Do not read the previous status through the still-open command overlay.
    await expect.poll(() => term.find("Commands")).toBeNull()
    value = pattern.exec(await term.text())?.[1]
    return value !== undefined && (expected === undefined || value === expected)
  }, { timeout: 30_000, message: `status never showed ${expected ?? pattern}` }).toBe(true)
  return value!
}

/** The daemon pid `/status` names, optionally waiting for the successor. */
async function statusPid(term: Tui, expected?: number): Promise<number> {
  return Number(await statusField(term, /Backend\s+daemon · pid (\d+)/, expected?.toString()))
}

/** The untitled open session's id from `/status`. */
async function statusSessionId(term: Tui): Promise<string> {
  return statusField(term, /Session\s+(hysec_\w+)/)
}

function appPids(workspace: Workspace): number[] {
  const escape = (value: string) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
  const pattern = `^([^ ]*/)?bun ${escape(tuiMain)}.*--dir ${escape(workspace.dir)}`
  const pids = execFileSync("pgrep", ["-f", pattern]).toString().trim().split("\n").map(Number)
  const supervisors = tuiInstances(pids)
  return pids.filter((pid) => !supervisors.includes(pid))
}

/** A second independent client on the selected browser or PTY driver. */
async function secondClient(tui: (command: string[], options?: import("./harness").LaunchOptions) => Promise<Tui>, workspace: Workspace): Promise<{ term: Tui }> {
  const [command, options] = selfLaunch(workspace)
  return { term: await tui(command, { ...options, independent: true }) }
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
    // Discovery is removed before the old process finishes shutting down.
    // Wait for process exit too; a replacement daemon would keep this nonempty.
    await expect.poll(() => servePids(workspace), { timeout: 20_000 }).toEqual([])

    // /reconnect starts the next daemon, reloads the session, and it works.
    await prompt(term, "/reconnect")
    let after = before
    await expect.poll(async () => {
      after = (await daemonStatus(workspace))?.pid ?? before
      return after
    }, { timeout: 30_000 }).not.toBe(before)
    expect(after).not.toBe(before)
    expect((await daemonStatus(workspace))?.pid).toBe(after)
    // Daemon discovery can precede the client finishing its reconnect.
    expect(await statusPid(term, after)).toBe(after)
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
    const previousApps = appPids(workspace)
    // Also exercise reconnect behavior at about 80 columns.
    await term.resize(690, 640)

    const restarted = await daemon(workspace, ["restart", "--json"])
    expect(restarted.code).toBe(0)
    const next = JSON.parse(restarted.stdout.trim()) as { pid: number; url: string; check?: unknown }
    // `serve restart` now blocks until the successor is healthy and returns
    // its ready record (hya-backend/src/serve.rs, restart handler).
    expect(next.pid).toBeDefined()
    expect(next.url).toBeDefined()
    let successorPid = before
    await expect.poll(async () => {
      successorPid = (await daemonStatus(workspace))?.pid ?? before
      return successorPid !== before
    }, { timeout: 30_000 }).toBe(true)
    expect(successorPid).not.toBe(before)
    await expect.poll(() => servePids(workspace)).toEqual([successorPid])
    await expect.poll(() => appPids(workspace), { timeout: 30_000 }).not.toEqual(previousApps)
    await term.waitForText("Before the stop.", 30_000)
    await prompt(term, "after the restart")
    await term.waitForText("After the new server.", 20_000)
    expect(await statusPid(term)).toBe(successorPid)
    expect(await statusPid(term, successorPid)).toBe(successorPid)
    await expect.poll(() => servePids(workspace)).toEqual([successorPid])
  })

  test("`hya serve restart` hot-updates the TUI too: a fresh TUI process on the same session keeps the unsent draft", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    await prompt(term, "first prompt")
    await term.waitForText("Before the stop.", 20_000)
    await term.waitForIdle()
    const session = await statusSessionId(term)
    const before = (await daemonStatus(workspace))!.pid
    const previousApps = appPids(workspace)
    expect(previousApps).toHaveLength(1)
    await prompt(term, "/layout show")
    await term.waitForText("Before the stop.")
    // Typed, not sent: the reload must hand it to the next TUI process.
    await term.type("unsent draft text")
    await term.waitForText("unsent draft text")

    const restarted = await daemon(workspace, ["restart", "--json"])
    expect(restarted.code).toBe(0)
    await expect.poll(async () => (await daemonStatus(workspace))?.pid ?? before, { timeout: 30_000 }).not.toBe(before)

    // Verify a fresh app process without relying on a removed status row.
    await expect.poll(() => appPids(workspace), { timeout: 30_000 }).not.toEqual(previousApps)
    await expect.poll(() => appPids(workspace)).toHaveLength(1)
    await term.attach(testInfo, "reloaded")
    await term.waitForText("unsent draft text")
    await term.waitForText("Before the stop.")
    // The draft is sent in the reloaded TUI, on the new daemon.
    await term.press("Enter")
    await term.waitForText("After the new server.", 20_000)
    expect(await statusSessionId(term)).toBe(session)
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

  test("two TUIs lose the daemon to a crash: exactly one starts the next, the other attaches to it", async ({ tui, workspace }, testInfo) => {
    const first = await tui(...selfLaunch(workspace))
    await first.waitForText("Message, !shell, or @file · / commands", 30_000)
    const { term: second } = await secondClient(tui, workspace)
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
      expect(await statusPid(first, pid)).toBe(pid)
      expect(await statusPid(second, pid)).toBe(pid)
      await second.attach(testInfo, "second-after")

      // Both still share live state: the second follows the first's session.
      await prompt(first, "shared after the move")
      await prompt(second, `/open ${session}`)
      await second.waitForText("shared after the move", 20_000)
    } finally {
      await second.close()
    }
  })
})
