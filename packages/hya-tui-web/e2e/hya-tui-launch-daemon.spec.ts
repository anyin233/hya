// Consolidated real-process lifecycle seam suite for the persistent backend
// daemon (ADR-0022, ADR-0023): auto-start, archive/resume/continue, empty
// session reaping, background exit, `hya serve stop` + `/reconnect`,
// `hya serve restart` hot-update preserving session & draft, multi-client live
// sync + SIGKILL crash recovery, launch errors, and cold startup tracing.

import { execFileSync } from "node:child_process"
import { chmod, readFile, writeFile } from "node:fs/promises"
import { createServer } from "node:net"
import { join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { Tui, terminalDriver } from "./harness"
import {
  api,
  createSession,
  daemon,
  daemonStatus,
  expect,
  expectStatus,
  fakeModelRef,
  hangStep,
  hyaBin,
  launchTest as test,
  selfLaunch,
  showStatusView,
  statusSessionId,
  textStep,
  tuiInstances,
  tuiMain,
  workspaceDb,
  type Backend,
  type Workspace,
} from "./hya"

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

async function statusField(term: Tui, pattern: RegExp, expected?: string): Promise<string> {
  let value: string | undefined
  await expect.poll(async () => {
    await showStatusView(term)
    const snapshot = await term.text()
    value = pattern.exec(snapshot)?.[1]
    return value !== undefined && (expected === undefined || value === expected)
  }, { timeout: 30_000, message: `status never showed ${expected ?? pattern}` }).toBe(true)
  return value!
}

async function statusPid(term: Tui, expected?: number): Promise<number> {
  return Number(await statusField(term, /Backend\s+daemon · pid (\d+)/, expected?.toString()))
}

async function backendOf(workspace: Workspace): Promise<Backend> {
  return { url: (await daemonStatus(workspace))!.url, dir: workspace.dir }
}

async function sessionInfo(backend: Backend, id: string): Promise<{ id: string; title?: string; archived?: boolean; busy?: boolean }> {
  return api(backend, "GET", `/v1/sessions/${id}`)
}

async function listedIds(url: string, query = ""): Promise<string[]> {
  const { sessions } = await (await fetch(`${url}/v1/sessions${query}`)).json() as { sessions?: { id: string }[] }
  return (sessions ?? []).map((row) => row.id)
}

function tuiPids(dir: string): number[] {
  return execFileSync("ps", ["-axo", "pid=,command="]).toString().split("\n")
    .map((line) => line.trim().split(/\s+/))
    .filter((argv) => argv[2] === tuiMain && argv.join(" ").includes(`--dir ${dir}`))
    .map((argv) => Number(argv[0]))
}

function servePids(workspace: Workspace): number[] {
  let out = ""
  try {
    const db = workspaceDb(workspace).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    out = execFileSync("pgrep", ["-f", `serve .*--db ${db}`]).toString()
  } catch {
    // pgrep exits 1 when nothing matches.
  }
  return out.split("\n").filter(Boolean).map(Number)
}

function tabTuis(workspace: Workspace): number[] {
  return execFileSync("ps", ["-axo", "pid=,command="]).toString().split("\n")
    .filter((line) => line.includes("--web-tab") && line.includes(workspace.dir) && !line.includes("--cwd"))
    .map((line) => Number(line.trim().split(/\s+/)[0]))
}

async function freePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const server = createServer()
    server.once("error", reject)
    server.listen(0, "127.0.0.1", () => {
      const address = server.address()
      const port = typeof address === "object" && address ? address.port : 0
      server.close(() => resolvePort(port))
    })
  })
}

const repoRoot = fileURLToPath(new URL("../../..", import.meta.url))
const externalHyaBin = !hyaBin.startsWith(repoRoot)
const checkoutPackageEnv: Record<string, string> = externalHyaBin
  ? { HYA_TUI_DIR: resolve(repoRoot, "packages/hya-tui"), HYA_TUI_WEB_DIR: resolve(repoRoot, "packages/hya-tui-web") }
  : {}

function bareHya(workspace: Workspace, port: number): [string[], { cwd: string; env: Record<string, string> }] {
  const { HYA_TUI_DIR: _tui, HYA_TUI_WEB_DIR: _web, ...env } = workspace.env
  return [[hyaBin, "--port", String(port)], { cwd: workspace.dir, env: { ...env, ...checkoutPackageEnv } }]
}

test.describe("daemon lifecycle, archive/resume, and empty-session cleanup", () => {
  test.use({ model: { steps: [textStep("Launched and replying."), textStep("Second reply."), textStep("Spare.")] } })

  test("auto-starts daemon, archives on /exit, skips archived on --continue, unarchives on --resume, and drops empty sessions", async ({ tui, workspace }) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    const id = await statusSessionId(term)
    const pid = await statusPid(term)
    const backend = await backendOf(workspace)
    // Seed an earlier session of the directory so --continue falls back to it.
    const { session: older } = await api<{ session: { id: string } }>(backend, "POST", "/v1/sessions", {
      agent: "hya-main", model: fakeModelRef, workdir: backend.dir,
    })
    await api(backend, "PATCH", `/v1/sessions/${older.id}`, { title: "Older work" })

    await prompt(term, "remember this")
    await term.waitForText("Launched and replying.", 20_000)
    await term.waitForIdle()

    // Create an empty session via /new, then /exit: empty session is reaped, daemon outlives client.
    const empty = await createSession(term, { previousId: id })
    await term.waitForText("No messages yet")
    expect(empty).not.toBe(id)
    expect(await listedIds(backend.url)).toContain(empty)
    await prompt(term, "/exit")
    expect(await term.waitForExit(20_000)).toBe(0)
    expect((await daemonStatus(workspace))?.pid).toBe(pid)
    await expect.poll(() => listedIds(backend.url), { timeout: 20_000 }).not.toContain(empty)

    // Reopen `id` and /exit to archive it; --continue skips archived and opens Older work.
    const toArchive = await tui(...selfLaunch(workspace, ["--resume", id]))
    await toArchive.waitForText("Launched and replying.", 30_000)
    await prompt(toArchive, "/exit")
    expect(await toArchive.waitForExit(20_000)).toBe(0)
    expect((await sessionInfo(backend, id)).archived).toBe(true)

    const cont = await tui(...selfLaunch(workspace, ["--continue"]))
    await expectStatus(cont, "Session", "Older work")
    expect(await statusPid(cont)).toBe(pid)

    // --resume <id> reopens and unarchives `id`.
    const resumed = await tui(...selfLaunch(workspace, ["--resume", id]))
    await resumed.waitForText("Launched and replying.", 30_000)
    expect((await sessionInfo(backend, id)).archived ?? false).toBe(false)
  })
})

test.describe("background exit and SIGKILL orphan cleanup", () => {
  test.use({ model: { steps: [hangStep(60_000), textStep("Spare.")] } })

  test("Ctrl+D leaves a running turn unarchived on the daemon, and a SIGKILLed TUI's empty session is reaped", async ({ tui, workspace, fakeModel }) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    const id = await statusSessionId(term)
    const backend = await backendOf(workspace)
    await prompt(term, "a long job")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await term.press("Control+d")
    expect(await term.waitForExit(20_000)).toBe(0)
    expect((await sessionInfo(backend, id)).busy).toBe(true)
    expect((await sessionInfo(backend, id)).archived ?? false).toBe(false)
    fakeModel!.release()
    await expect.poll(async () => (await sessionInfo(backend, id)).busy ?? false, { timeout: 20_000 }).toBe(false)
    expect(await listedIds(backend.url)).toContain(id)

    // A second TUI opens an empty session and gets SIGKILLed: daemon reaps the empty session.
    const victim = await tui(...selfLaunch(workspace))
    await victim.waitForText("a long job", 30_000)
    const empty = await createSession(victim, { previousId: id })
    expect(empty).not.toBe(id)
    await victim.waitForText("No messages yet")
    expect(await listedIds(backend.url)).toContain(empty)
    const supervisors = tuiInstances(tuiPids(workspace.dir))
    expect(supervisors).toHaveLength(1)
    process.kill(supervisors[0]!, "SIGKILL")
    await expect.poll(() => tuiPids(workspace.dir).length, { timeout: 10_000 }).toBe(0)
    await expect.poll(() => listedIds(backend.url), { timeout: 20_000 }).not.toContain(empty)
  })
})

test.describe("daemon stop, restart hot-update, and multi-client crash recovery", () => {
  test.use({ model: { steps: [textStep("Before the stop."), textStep("After the new server."), textStep("Reply seen by both."), textStep("Spare.")] } })

  test("`hya serve stop` keeps the TUI stopped until `/reconnect` starts a successor daemon", async ({ tui, workspace }) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    const before = await statusPid(term)
    await prompt(term, "first prompt")
    await term.waitForText("Before the stop.", 20_000)
    await term.waitForIdle()

    const stopped = await daemon(workspace, ["stop"])
    expect(stopped.code).toBe(0)
    await expect.poll(() => daemonStatus(workspace), { timeout: 20_000 }).toBeUndefined()
    await expect.poll(() => servePids(workspace), { timeout: 20_000 }).toEqual([])

    // Prompts are refused while stopped and do not auto-start a daemon.
    await prompt(term, "lost prompt")
    await expect.poll(() => term.find("lost prompt")).toBeNull()
    expect(await daemonStatus(workspace)).toBeUndefined()

    // /reconnect starts the successor daemon and restores prompt execution.
    await prompt(term, "/reconnect")
    let after = before
    await expect.poll(async () => {
      after = (await daemonStatus(workspace))?.pid ?? before
      return after
    }, { timeout: 30_000 }).not.toBe(before)
    expect(await statusPid(term, after)).toBe(after)
    await prompt(term, "second prompt")
    await term.waitForText("After the new server.", 20_000)
  })

  test("`hya serve restart` hot-updates the TUI supervisor preserving session and unsent draft", async ({ tui, workspace }) => {
    const trace = join(workspace.dir, "reload.jsonl")
    const term = await tui([hyaBin, "--port", "0"], {
      cwd: workspace.dir,
      env: { ...workspace.env, HYA_TUI_DIR: resolve("../hya-tui"), HYA_TUI_WEB_DIR: resolve("."), HYA_STARTUP_TRACE_FILE: trace },
    })
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    await prompt(term, "first prompt")
    await term.waitForText("Before the stop.", 20_000)
    await term.waitForIdle()
    const session = await statusSessionId(term)
    const before = (await daemonStatus(workspace))!.pid

    // Typed, not sent: hot-update must hand the draft to the reloaded TUI process.
    await term.type("preserved draft")
    await term.waitForText("preserved draft")

    const restarted = await daemon(workspace, ["restart", "--json"])
    expect(restarted.code).toBe(0)
    await expect.poll(async () => (await daemonStatus(workspace))?.pid ?? before, { timeout: 30_000 }).not.toBe(before)
    await expect.poll(async () => (await readFile(trace, "utf8")).match(/tui_app_entry/g)?.length, { timeout: 30_000 }).toBe(2)
    expect((await readFile(trace, "utf8")).match(/tui_supervisor_entry/g)).toHaveLength(1)

    await term.waitForText("preserved draft")
    await term.waitForText("Before the stop.")
    await term.press("Enter")
    await term.waitForText("After the new server.", 20_000)
    expect(await statusSessionId(term)).toBe(session)
    await term.press("Control+d")
    expect(await term.waitForExit(20_000)).toBe(0)
    expect(await daemonStatus(workspace)).toBeDefined()
  })

  test("two TUIs share one daemon live and elect a single successor after SIGKILL crash", async ({ tui, workspace }) => {
    const first = await tui(...selfLaunch(workspace))
    await first.waitForText("Message, !shell, or @file · / commands", 30_000)
    const [command, options] = selfLaunch(workspace)
    const second = await tui(command, { ...options, independent: true })
    try {
      await second.waitForText("Message, !shell, or @file · / commands", 30_000)
      const before = await statusPid(first)
      expect(await statusPid(second)).toBe(before)
      const session = await statusSessionId(first)

      // Live sharing across two TUIs on one database daemon.
      await prompt(second, `/open ${session}`)
      await expectStatus(second, "Session", session)
      await prompt(first, "/rename Shared across TUIs")
      await expectStatus(second, "Session", "Shared across TUIs")

      // Unexpected daemon death (SIGKILL): exactly one successor starts, both attach and stay live.
      process.kill(before, "SIGKILL")
      let after = before
      await expect.poll(async () => {
        after = (await daemonStatus(workspace))?.pid ?? before
        return after
      }, { timeout: 30_000 }).not.toBe(before)
      expect(await statusPid(first, after)).toBe(after)
      expect(await statusPid(second, after)).toBe(after)
      expect(servePids(workspace)).toEqual([after])

      await prompt(second, "/layout show")
      await prompt(first, "shared after crash")
      await second.waitForText("shared after crash", 20_000)
      await second.waitForText("Before the stop.", 20_000)
      await prompt(second, "/exit")
      await expect.poll(() => second.exitStatus(), { timeout: 15_000 }).toBe(0)
      expect(alive(after)).toBe(true)
    } finally {
      await second.close()
    }
  })
})

test.describe("launch errors and cold startup trace", () => {
  test("missing binary and failing daemon report clear errors, and cold startup loads extensions without failure", async ({ tui, workspace }) => {
    const missing = await tui(...selfLaunch(workspace, [], { env: { HYA_BIN: join(workspace.root, "no-such-hya") } }))
    await missing.waitForText("could not reach or start the hya server: hya binary not found: HYA_BIN=")
    expect(await missing.waitForExit()).toBe(1)

    const broken = join(workspace.root, "broken-hya")
    await writeFile(broken, "#!/bin/sh\necho 'error: config is broken at line 3' >&2\nexit 2\n")
    await chmod(broken, 0o755)
    const failed = await tui(...selfLaunch(workspace, [], { env: { HYA_BIN: broken } }))
    await failed.waitForText("hya serve start exited with code 2")
    await failed.waitForText("error: config is broken at line 3")
    expect(await failed.waitForExit()).toBe(1)

    // Cold startup trace via bare hya: all-bundle startup reaches ready and loads extensions.
    const trace = join(workspace.dir, "cold.jsonl")
    const term = await tui(["bun", resolve("e2e/fixtures/startup-launch.ts"), hyaBin, "--port", "0"], {
      cwd: workspace.dir,
      env: {
        ...workspace.env,
        HYA_TUI_DIR: resolve("../hya-tui"),
        HYA_TUI_WEB_DIR: resolve("."),
        HYA_STARTUP_TRACE_FILE: trace,
      },
    })
    await term.waitForText("Message, !shell, or @file · / commands", 60_000)
    await expect.poll(async () => await readFile(trace, "utf8").catch(() => ""), { timeout: 30_000 }).toMatch(/tui_extensions_(loaded|failed)/)
    const raw = await readFile(trace, "utf8")
    expect(raw).not.toContain("tui_extensions_failed")
    expect(raw).not.toContain("Connection failed")
    await term.press("Control+d")
    expect(await term.waitForExit(20_000)).toBe(0)
  })
})

test.describe("WebUI tabs (bare hya)", { tag: "@browser-only" }, () => {
  test.skip(terminalDriver === "pty", "Browser tab disconnect and WebUI-only commands require Chromium")
  test.use({ model: { steps: [hangStep(60_000), textStep("Spare.")] } })

  test("a tab offers no /to-background, Ctrl+D only shows a notice, and closing the tab leaves the session unarchived and running", async ({ tui, workspace, fakeModel }) => {
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
    await web.type("/to")
    await web.waitForText("/todos")
    expect(await web.text()).not.toContain("/to-background")
    await web.press("Escape")

    await prompt(web, "a long web job")
    await expect.poll(() => fakeModel!.pendingHangs(), { timeout: 20_000 }).toBe(1)
    await webPage.close()
    await expect.poll(() => tabTuis(workspace).length, { timeout: 20_000 }).toBe(0)
    const left = await sessionInfo(backend, id)
    expect(left.archived ?? false).toBe(false)
    expect(left.busy).toBe(true)
    fakeModel!.release()
    await expect.poll(async () => (await sessionInfo(backend, id)).busy ?? false, { timeout: 20_000 }).toBe(false)
  })
})
