// `/connect-remote` and `/disconnect-remote` (docs/tui.md "Remote backends"):
// a real `hya proxy`, a real `hya serve --relay … --relay-ephemeral` behind
// it, and a TUI on its own local daemon that moves to the remote through its
// `hya bridge` child and back. The link is a secret: it must not be left in
// the terminal after it was submitted.

import { spawn, type ChildProcess } from "node:child_process"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { Tui } from "./harness"
import { api, expect, expectStatus, hyaBin, launchTest, selfLaunch, statusSessionId } from "./hya"

type Remote = { url: string; link: string; relay: string; root: string; stopServe: () => Promise<void> }

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
      const serve = children[children.length - 1]!
      const stopServe = async (): Promise<void> => {
        if (serve.exitCode !== null) return
        const exited = new Promise((resolve) => serve.once("exit", resolve))
        serve.kill("SIGTERM")
        await exited
      }
      const url = /hya server listening on (\S+)/.exec(served.input)?.[1]
      if (!url) throw new Error("remote backend did not announce its listener")
      await use({ url, link: served[1]!, relay: relay.replace(/^http:\/\//, ""), root: env.work!, stopServe })
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

const secretOf = (link: string): string => link.slice(link.indexOf("#") + 1)

test.describe("/connect-remote", () => {
  test("connects through real proxy + bridge, works over the relay without leaking secrets, and /disconnect-remote returns to local", async ({ tui, workspace, remote }) => {
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    await statusSessionId(term)

    // Connect via pasted concealed entry (`hya bridge` child handles the Noise tunnel).
    await prompt(term, "/connect-remote")
    await term.waitForText("Relay link")
    await term.paste(remote.link)
    await term.waitForText(`${"•".repeat(32)}…  ${remote.link.length} characters`)
    expect(await term.find(secretOf(remote.link))).toBeNull()
    await term.press("Enter")
    await term.waitForText("No projects yet · n creates one", 30_000)
    expect(await term.find(secretOf(remote.link))).toBeNull()

    // Create a Project on the remote host, switch into it, and run a turn over the relay.
    await term.press("n")
    await term.waitForText("New project name")
    await term.type("remote-project")
    await term.press("Enter")
    await term.waitForText("Root 1 (primary)")
    await term.type(remote.root)
    await term.press("Enter")
    await term.waitForText("Root 2 (Enter empty to finish)")
    await term.press("Enter")
    await term.waitForText(/Created remote-project/)
    await term.press("Enter")
    await expect.poll(() => term.find("Created remote-project")).toBeNull()

    await prompt(term, "/status")
    await term.waitForText(`remote: ${remote.relay}`)
    await prompt(term, "/layout show")
    await prompt(term, "hello over the relay")
    await term.waitForText("No live provider is available", 20_000)

    // Disconnect returns to the local daemon.
    await prompt(term, "/disconnect-remote")
    await expectStatus(term, "Server", /^http:\/\/127\.0\.0\.1:\d+$/)
    await statusSessionId(term)
    expect(await term.find("remote-project")).toBeNull()
    expect(await term.find(secretOf(remote.link))).toBeNull()
  })

  test("resolves remote @path image attachments on the remote root and reports remote offline 503 cleanly", async ({ tui, workspace, remote }) => {
    const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
    await mkdir(join(remote.root, "shots"), { recursive: true })
    await writeFile(join(remote.root, "shots", "remote-ui.png"), Buffer.concat([signature, Buffer.alloc(3 * 1024, 1)]))
    await writeFile(join(workspace.dir, "local-only.png"), Buffer.concat([signature, Buffer.alloc(64, 1)]))
    const term = await tui(...selfLaunch(workspace))
    await term.waitForText("Message, !shell, or @file · / commands", 30_000)
    await statusSessionId(term)
    await term.type(`/connect-remote ${remote.link}`)
    await term.press("Enter")
    await term.waitForText("No projects yet · n creates one", 30_000)
    await term.press("n")
    await term.waitForText("New project name")
    await term.type("remote-project")
    await term.press("Enter")
    await term.waitForText("Root 1 (primary)")
    await term.type(remote.root)
    await term.press("Enter")
    await term.waitForText("Root 2 (Enter empty to finish)")
    await term.press("Enter")
    await term.waitForText(/Created remote-project/)
    await term.press("Enter")
    await expect.poll(() => term.find("Created remote-project")).toBeNull()

    const session = await statusSessionId(term)
    const readMessages = async () => (await api<{ messages: unknown[] }>({ url: remote.url, dir: remote.root }, "GET", `/v2/sessions/${session}/messages`)).messages
    await term.type("@shots/remote-ui.png what is this")
    await term.waitForText("[image] remote-ui.png · 3 KB", 10_000)
    await term.press("Enter")
    await term.waitForText(/attachment . remote-ui\.png/, 20_000)

    await term.type("@local-only.png ")
    await term.waitForText("[image] local-only.png · file not found", 10_000)
    await term.press("Enter")
    await term.press("Control+c")
    await prompt(term, "after refused")
    await expect.poll(async () => JSON.stringify(await readMessages())).toContain("after refused")
    expect(JSON.stringify(await readMessages())).not.toContain("local-only.png")

    // Stopping the remote server makes Project view actions fail with one clean 503 status line.
    await remote.stopServe()
    await prompt(term, "/project")
    await term.waitForText("Refresh failed: unavailable: remote backend is offline", 20_000)
  })
})
