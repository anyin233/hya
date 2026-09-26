// `/connect-remote` and `/disconnect-remote` (docs/tui.md "Remote backends"):
// a real `hya proxy`, a real `hya serve --relay … --relay-ephemeral` behind
// it, and a TUI on its own local daemon that moves to the remote through its
// `hya bridge` child and back. The link is a secret: it must not be left in
// the terminal after it was submitted.

import { spawn, type ChildProcess } from "node:child_process"
import { mkdir, mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hyaBin, launchTest, selfLaunch } from "./hya"

type Remote = { link: string; relay: string; root: string }

/** Spawn `hya <args>` and resolve with the first match of `pattern` in its output. */
function spawnUntil(args: string[], pattern: RegExp, env: Record<string, string>, children: ChildProcess[]): Promise<RegExpExecArray> {
  const { HYA_MODEL: _model, ...inherited } = process.env
  const child = spawn(hyaBin, args, { env: { ...inherited, ...env }, stdio: ["ignore", "pipe", "pipe"] })
  children.push(child)
  return new Promise((resolve, reject) => {
    let output = ""
    const onData = (chunk: Buffer) => {
      output += chunk.toString()
      const match = pattern.exec(output)
      if (match) resolve(match)
    }
    child.stdout!.on("data", onData)
    child.stderr!.on("data", onData)
    child.once("error", reject)
    child.once("exit", (code) => reject(new Error(`hya ${args[0]} exited (${code})`)))
  })
}

const test = launchTest.extend<{ remote: Remote }>({
  remote: async ({}, use) => {
    const root = await mkdtemp(join(tmpdir(), "hya-tui-remote-"))
    const env: Record<string, string> = {}
    for (const name of ["home", "config", "data", "state", "cache", "work"]) await mkdir((env[name] = join(root, name)), { recursive: true })
    const isolated = { HOME: env.home!, XDG_CONFIG_HOME: env.config!, XDG_DATA_HOME: env.data!, XDG_STATE_HOME: env.state!, XDG_CACHE_HOME: env.cache! }
    const children: ChildProcess[] = []
    try {
      const proxy = await spawnUntil(["proxy", "--host", "127.0.0.1", "--port", "0"], /hya proxy listening on (http:\/\/\S+)/, isolated, children)
      const relay = proxy[1]!
      const served = await spawnUntil(
        ["serve", "--bind", "127.0.0.1:0", "--db", join(root, "remote.db"), "--relay", relay, "--relay-ephemeral"],
        /hya relay link: (\S+)/,
        isolated,
        children,
      )
      await use({ link: served[1]!, relay: relay.replace(/^http:\/\//, ""), root: env.work! })
    } finally {
      for (const child of children.reverse()) {
        if (child.exitCode !== null) continue
        const exited = new Promise((resolve) => child.once("exit", resolve))
        child.kill("SIGTERM")
        const timer = setTimeout(() => child.kill("SIGKILL"), 3_000)
        await exited
        clearTimeout(timer)
      }
      await rm(root, { recursive: true, force: true })
    }
  },
})

async function prompt(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

/** The link's secret part (after `#`): it must never stay on screen. */
const secretOf = (link: string): string => link.slice(link.indexOf("#") + 1)

test.describe("/connect-remote", () => {
  test("connects through the relay, works there, and /disconnect-remote comes back to the local backend", async ({ tui, workspace, remote }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Connected to hya", 30_000)
    await term.waitForText(/hya · hysec_\w+/)
    const room = /\/([a-z0-9]+)#/.exec(remote.link)![1]!
    expect(room.length).toBeGreaterThan(0)

    // Typed inline (the concealed entry is the other way, below).
    await term.type(`/connect-remote ${remote.link}`)
    await term.press("Enter")
    await term.waitForText("No projects yet · n creates one", 30_000)
    expect(await term.find(secretOf(remote.link)), "the link's secret is gone from the screen").toBeNull()
    expect(await term.find("hya+insecure://"), "no link on screen").toBeNull()

    // The remote's Project view: create a Project (its root lives on the remote machine), then switch into it.
    await term.press("n")
    await term.type("remote-project")
    await term.press("Enter")
    await term.type(remote.root)
    await term.press("Enter")
    await term.press("Enter")
    await term.waitForText(/Created remote-project/)
    await term.press("Enter")
    await term.waitForText(/Project remote-project/)
    // The header names the remote (cut to fit), never the bridge's loopback URL.
    await term.waitForText(`remote: ${remote.relay}/`)

    // A prompt over the relay, answered by the remote's offline model.
    await prompt(term, "hello over the relay")
    await term.waitForText("No live provider is available", 20_000)
    await term.attach(testInfo, "remote-session")

    // Up recalls the command without its link.
    await term.press("ArrowUp")
    await term.press("ArrowUp")
    await term.waitForText("/connect-remote")
    expect(await term.find(secretOf(remote.link))).toBeNull()
    // Run again, the recalled command asks for the link in the concealed entry; Esc cancels it.
    await term.press("Enter")
    await term.waitForText("Relay link")
    await term.press("Escape")
    await term.waitForText("Not connected · /connect-remote cancelled")

    await prompt(term, "/disconnect-remote")
    await term.waitForText("Back on the local backend", 30_000)
    await expect.poll(() => term.find(`remote: ${remote.relay}/`)).toBeNull()
    await term.waitForText(/hya · hysec_\w+ · .* · http:\/\/127\.0\.0\.1:\d+/)
    expect(await term.find("remote-project")).toBeNull()
    expect(await term.find(secretOf(remote.link))).toBeNull()
  })

  test("without a link it asks for one in a concealed entry; Esc cancels, a pasted link connects", async ({ tui, workspace, remote }, testInfo) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Connected to hya", 30_000)

    await prompt(term, "/connect-remote")
    await term.waitForText("Relay link")
    await term.waitForText("paste or type the relay link (hidden) · Enter connects · Esc cancels")
    await term.type("hya+insecure://x#abc")
    await term.waitForText(`${"•".repeat(20)}  20 characters`)
    expect(await term.find("hya+insecure://")).toBeNull()
    await term.press("Escape")
    await term.waitForText("Not connected · /connect-remote cancelled")
    await expect.poll(() => term.find("Relay link")).toBeNull()

    await prompt(term, "/connect-remote")
    await term.waitForText("Relay link")
    await term.page.evaluate((text) => window.hyaTerm.term.paste(text), remote.link)
    await term.waitForText(`${"•".repeat(32)}…  ${remote.link.length} characters`)
    expect(await term.find(secretOf(remote.link))).toBeNull()
    await term.attach(testInfo, "concealed-entry")
    await term.press("Enter")
    await term.waitForText("No projects yet · n creates one", 30_000)
    await term.press("Escape")
    await term.waitForText(/hya · no session · remote: /)
    expect(await term.find(secretOf(remote.link))).toBeNull()
  })

  test("the concealed entry fits about 80 columns", async ({ tui, workspace, remote }) => {
    const term = await tui(...selfLaunch(workspace, [], { viewport: { width: 690, height: 640 } }))
    await term.waitForText("Connected to hya", 30_000)
    await prompt(term, "/connect-remote")
    await term.waitForText("paste or type the relay link (hidden) · Enter connects · Esc cancels")
    await term.page.evaluate((text) => window.hyaTerm.term.paste(text), remote.link)
    await term.waitForText(`${"•".repeat(32)}…  ${remote.link.length} characters`)
    const lines = await term.lines()
    const top = lines.findIndex((line) => line.includes("Relay link"))
    expect(lines[top]!.trimEnd().endsWith("┐"), "the entry's border closes inside the terminal").toBe(true)
    expect(await term.find(secretOf(remote.link))).toBeNull()
  })

  test("a link that the remote rejects is reported, and the TUI stays on its local backend", async ({ tui, workspace, remote }) => {
    const term = await tui(...selfLaunch(workspace, [], { viewport: { width: 690, height: 640 } }))
    await term.waitForText("Connected to hya", 30_000)
    await term.waitForText(/hya · hysec_\w+/)
    // Same relay and room, a well-formed but wrong secret: the remote rejects the handshake.
    const at = remote.link.length - 10
    const wrong = `${remote.link.slice(0, at)}${remote.link[at] === "A" ? "B" : "A"}${remote.link.slice(at + 1)}`
    await term.type(`/connect-remote ${wrong}`)
    await term.press("Enter")
    await term.waitForText("Remote connection failed: the remote backend rejected the relay link", 30_000)
    expect(await term.find(secretOf(wrong).slice(0, 12))).toBeNull()
    await term.waitForText(/hya · hysec_\w+ · .* · http:\/\/127\.0\.0\.1:\d+/)
  })
})
