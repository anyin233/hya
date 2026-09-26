// One-command launch (docs/tui.md "Start it"; ADR-0023): without `--server`
// the TUI finds the `hya` binary (`HYA_BIN` here) and uses the backend daemon
// of its database: the one already running, else one it starts with
// `hya serve start` (detached, in `--dir`). The daemon outlives the TUI.
// `--continue` reopens the most recent session that is not archived; a plain
// start opens a new ephemeral session that the daemon drops again once it is
// still empty and no TUI shows it (after `/exit`, or a `kill -9`). A
// missing binary or a daemon that fails to start is reported with its
// output tail.

import { execFileSync } from "node:child_process"
import { chmod, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { daemonStatus, expect, launchTest as test, selfLaunch, textStep, tuiMain } from "./hya"

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
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

/** The daemon's pid, read from `/status`; also checks it is a running `hya serve`. */
async function backendPid(term: Tui): Promise<number> {
  await prompt(term, "/status")
  await term.waitForText(/Backend\s+daemon · pid \d+ · db \//)
  // The row wraps in the status view; the start time is on it too.
  await term.waitForText(/started \d+[sm] ago/)
  const pid = Number(/Backend\s+daemon · pid (\d+)/.exec(await term.text())![1])
  expect(alive(pid)).toBe(true)
  expect(execFileSync("ps", ["-o", "command=", "-p", String(pid)]).toString()).toContain("serve --bind 127.0.0.1:0")
  return pid
}

/** Ids of the root sessions the daemon at `url` lists. */
async function listed(url: string): Promise<string[]> {
  return ((await (await fetch(`${url}/v1/sessions`)).json()) as { sessions?: { id: string }[] }).sessions?.map((row) => row.id) ?? []
}

/** Pids of the TUIs (`bun <tui main> --dir <dir> …`) running for `dir`. */
function tuiPids(dir: string): number[] {
  return execFileSync("ps", ["-axo", "pid=,command="]).toString().split("\n")
    .map((line) => line.trim().split(/\s+/))
    .filter((argv) => argv[2] === tuiMain && argv.join(" ").includes(`--dir ${dir}`))
    .map((argv) => Number(argv[0]))
}

async function healthy(url: string): Promise<boolean> {
  try {
    const response = await fetch(`${url}/v1/health`, { signal: AbortSignal.timeout(2_000) })
    return ((await response.json()) as { ok?: boolean }).ok === true
  } catch {
    return false
  }
}

test.describe("one-command launch", () => {
  test.use({ model: { steps: [textStep("Launched and replying."), textStep("Second reply."), textStep("Third reply.")] } })

  test("starts the database's daemon, answers a prompt end to end; /exit leaves the daemon running and the next TUI uses it", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Connected to hya", 30_000)
    // No --server: the header shows the daemon's local URL.
    await term.waitForText(/http:\/\/127\.0\.0\.1:\d+/)
    await prompt(term, "hello")
    await term.waitForText("Launched and replying.", 20_000)
    await term.waitForText(/^Ready/m)
    const pid = await backendPid(term)
    await term.attach(testInfo, "status")
    await prompt(term, "/exit")
    expect(await term.waitForExit()).toBe(0)

    // The daemon outlives its client.
    const running = await daemonStatus(workspace)
    expect(running?.pid).toBe(pid)
    expect(await healthy(running!.url)).toBe(true)

    // A new TUI attaches to it instead of starting another.
    const next = await tui(...selfLaunch(workspace))
    await next.waitForText("Connected to hya", 30_000)
    expect(await backendPid(next)).toBe(pid)
    await prompt(next, "/exit")
    expect(await next.waitForExit()).toBe(0)
  })

  test("a plain start opens a new session; an empty one is dropped by the daemon after exit, one with messages kept; --continue reopens it", async ({ tui, workspace }) => {
    // Ctrl+D quits without archiving (`/exit` would archive it, and --continue skips archived sessions).
    const first = await tui(...selfLaunch(workspace))
    await first.waitForText("Connected to hya", 30_000)
    // Created on connect: the header names it before anything is typed.
    await first.waitForText(/hya · hysec_\w+ · build/)
    await first.waitForText("No messages yet")
    await prompt(first, "remember this")
    await first.waitForText("Launched and replying.", 20_000)
    await first.waitForText(/^Ready/m)
    await first.press("Control+d")
    await first.waitForExit()

    // A fresh start gets its own new, empty session…
    const fresh = await tui(...selfLaunch(workspace))
    await fresh.waitForText("Connected to hya", 30_000)
    await fresh.waitForText("No messages yet")
    const empty = /hya · (hysec_\w+)/.exec(await fresh.text())![1]!
    const url = (await daemonStatus(workspace))!.url
    expect(await listed(url)).toContain(empty)
    await prompt(fresh, "/exit")
    expect(await fresh.waitForExit()).toBe(0)
    // …which the daemon drops once no client shows it (after a short grace):
    // only the session with messages is left.
    await expect.poll(() => listed(url), { timeout: 20_000 }).not.toContain(empty)
    expect(await listed(url)).toHaveLength(1)

    const resumed = await tui(...selfLaunch(workspace, ["--continue"]))
    await resumed.waitForText("Connected to hya", 30_000)
    await resumed.waitForText("remember this")
    await resumed.waitForText("Launched and replying.")
  })

  test("a killed TUI's empty session is dropped by the daemon too", async ({ tui, workspace }) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Connected to hya", 30_000)
    await term.waitForText("No messages yet")
    const empty = /hya · (hysec_\w+)/.exec(await term.text())![1]!
    const url = (await daemonStatus(workspace))!.url
    expect(await listed(url)).toContain(empty)
    // No exit handler runs: only its closed session stream tells the daemon.
    const pids = tuiPids(workspace.dir)
    expect(pids).toHaveLength(1)
    process.kill(pids[0]!, "SIGKILL")
    await expect.poll(() => tuiPids(workspace.dir).length, { timeout: 10_000 }).toBe(0)
    await expect.poll(() => listed(url), { timeout: 20_000 }).not.toContain(empty)
  })

  test("closing the browser tab (SIGHUP) leaves the daemon running", async ({ tui, workspace, page }) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Connected to hya", 30_000)
    const pid = await backendPid(term)
    await page.goto("about:blank")
    // The tab's TUI is gone (`bun <tui main> --dir …`; the host's own argv names it later on)…
    const tuis = () => execFileSync("ps", ["-axo", "command="]).toString().split("\n")
      .filter((line) => line.trim().split(/\s+/)[1] === tuiMain && line.includes(`--dir ${workspace.dir}`))
    await expect.poll(() => tuis().length, { timeout: 15_000 }).toBe(0)
    // …and the daemon still runs.
    expect(alive(pid)).toBe(true)
    expect((await daemonStatus(workspace))?.pid).toBe(pid)
  })
})

test.describe("launch errors", () => {
  test("a missing binary is a clear error and exit status 1", async ({ tui, workspace }) => {
    const term = await tui(...selfLaunch(workspace, [], { env: { HYA_BIN: join(workspace.root, "no-such-hya") } }))
    await term.waitForText("could not reach or start the hya server: hya binary not found: HYA_BIN=")
    await term.waitForText("does not exist")
    expect(await term.waitForExit()).toBe(1)
  })

  test("a daemon that fails to start shows the exit code and output tail", async ({ tui, workspace }) => {
    const broken = join(workspace.root, "broken-hya")
    await writeFile(broken, "#!/bin/sh\necho 'error: config is broken at line 3' >&2\nexit 2\n")
    await chmod(broken, 0o755)
    const term = await tui(...selfLaunch(workspace, [], { env: { HYA_BIN: broken } }))
    await term.waitForText("hya serve start exited with code 2")
    await term.waitForText("error: config is broken at line 3")
    expect(await term.waitForExit()).toBe(1)
  })
})
