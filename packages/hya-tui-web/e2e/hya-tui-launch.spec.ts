// One-command launch (docs/tui.md "Start it"; Tier 1 G31): without
// `--server` the TUI finds the `hya` binary (`HYA_BIN` here), starts
// `hya serve` on a free port in `--dir`, and stops it on every way out.
// `--continue` reopens the most recent session; a missing binary or a
// server that fails to start is reported with its output tail.

import { execFileSync } from "node:child_process"
import { chmod, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, launchTest as test, selfLaunch, textStep } from "./hya"

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

/** The started backend's pid, read from `/status`; also checks it is a running `hya serve`. */
async function backendPid(term: Tui): Promise<number> {
  await prompt(term, "/status")
  await term.waitForText(/Backend\s+started by this TUI · pid \d+/)
  const pid = Number(/pid (\d+)/.exec(await term.text())![1])
  expect(alive(pid)).toBe(true)
  expect(execFileSync("ps", ["-o", "command=", "-p", String(pid)]).toString()).toContain("serve --bind 127.0.0.1:0")
  return pid
}

async function gone(pid: number): Promise<void> {
  await expect.poll(() => alive(pid), { timeout: 15_000, message: `hya serve pid ${pid} still running` }).toBe(false)
}

test.describe("one-command launch", () => {
  test.use({ model: { steps: [textStep("Launched and replying."), textStep("Second reply.")] } })

  test("starts its own backend, answers a prompt end to end, and /exit stops it", async ({ tui, workspace }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Connected to hya", 30_000)
    // No --server: the header shows the started backend's local URL.
    await term.waitForText(/http:\/\/127\.0\.0\.1:\d+/)
    await prompt(term, "hello")
    await term.waitForText("Launched and replying.", 20_000)
    await term.waitForText(/^Ready/m)
    const pid = await backendPid(term)
    await term.attach(testInfo, "status")
    await prompt(term, "/exit")
    expect(await term.waitForExit()).toBe(0)
    await gone(pid)
  })

  test("--continue reopens the most recent session of the directory", async ({ tui, workspace }) => {
    const first = await tui(...selfLaunch(workspace))
    await first.waitForText("Connected to hya", 30_000)
    await prompt(first, "remember this")
    await first.waitForText("Launched and replying.", 20_000)
    await first.waitForText(/^Ready/m)
    await prompt(first, "/exit")
    await first.waitForExit()

    // A plain launch starts fresh: no session is open.
    const fresh = await tui(...selfLaunch(workspace))
    await fresh.waitForText("Connected to hya", 30_000)
    await fresh.waitForText("No messages yet")
    await fresh.waitForText("· no session")
    await prompt(fresh, "/exit")
    await fresh.waitForExit()

    const resumed = await tui(...selfLaunch(workspace, ["--continue"]))
    await resumed.waitForText("Connected to hya", 30_000)
    await resumed.waitForText("remember this")
    await resumed.waitForText("Launched and replying.")
  })

  test("closing the browser tab (SIGHUP) stops the backend too", async ({ tui, workspace, page }) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Connected to hya", 30_000)
    const pid = await backendPid(term)
    await page.goto("about:blank")
    await gone(pid)
  })
})

test.describe("launch errors", () => {
  test("a missing binary is a clear error and exit status 1", async ({ tui, workspace }) => {
    const term = await tui(...selfLaunch(workspace, [], { env: { HYA_BIN: join(workspace.root, "no-such-hya") } }))
    await term.waitForText("could not start the backend: hya binary not found: HYA_BIN=")
    await term.waitForText("does not exist")
    expect(await term.waitForExit()).toBe(1)
  })

  test("a server that fails to start shows its exit code and output tail", async ({ tui, workspace }) => {
    const broken = join(workspace.root, "broken-hya")
    await writeFile(broken, "#!/bin/sh\necho 'error: config is broken at line 3' >&2\nexit 2\n")
    await chmod(broken, 0o755)
    const term = await tui(...selfLaunch(workspace, [], { env: { HYA_BIN: broken } }))
    await term.waitForText("exited with code 2 before it was ready")
    await term.waitForText("error: config is broken at line 3")
    expect(await term.waitForExit()).toBe(1)
  })
})
