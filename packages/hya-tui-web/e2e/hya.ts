// hya-specific fixtures: an isolated `hya serve` on the offline echo model and
// the argv that runs packages/hya-tui against it. The host itself stays
// generic; only these specs know about hya.

import { spawn, type ChildProcess } from "node:child_process"
import { existsSync } from "node:fs"
import { mkdir, mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { test as base, type Tui } from "./harness"

const repoRoot = fileURLToPath(new URL("../../..", import.meta.url))

/** `hya` binary under test: `HYA_BIN`, else the workspace debug build. */
export const hyaBin = process.env.HYA_BIN ?? join(repoRoot, "target/debug/hya")

export type Backend = {
  url: string
  /** Isolated workspace directory, passed to the TUI as `--dir`. */
  dir: string
}

async function startBackend(root: string): Promise<{ child: ChildProcess; backend: Backend }> {
  const dir = join(root, "work")
  const env: Record<string, string> = {}
  for (const name of ["home", "config", "data", "state", "cache"]) {
    env[name] = join(root, name)
    await mkdir(env[name]!, { recursive: true })
  }
  await mkdir(dir, { recursive: true })
  const { HYA_MODEL: _model, ...inherited } = process.env
  const child = spawn(hyaBin, ["serve", "--bind", "127.0.0.1:0", "--db", join(root, "hya.db")], {
    cwd: dir,
    env: {
      ...inherited,
      HOME: env.home!,
      XDG_CONFIG_HOME: env.config!,
      XDG_DATA_HOME: env.data!,
      XDG_STATE_HOME: env.state!,
      XDG_CACHE_HOME: env.cache!,
    },
    stdio: ["ignore", "pipe", "pipe"],
  })
  return new Promise((resolve, reject) => {
    let output = ""
    const onData = (chunk: Buffer) => {
      output += chunk.toString()
      const match = /hya server listening on (\S+)/.exec(output)
      if (match) resolve({ child, backend: { url: match[1]!, dir } })
    }
    child.stdout!.on("data", onData)
    child.stderr!.on("data", (chunk: Buffer) => (output += chunk.toString()))
    child.once("error", reject)
    child.once("exit", (code) => reject(new Error(`hya serve exited (${code}): ${output.slice(-2000)}`)))
  })
}

export const test = base.extend<{ backend: Backend }>({
  backend: async ({}, use) => {
    if (!existsSync(hyaBin)) {
      throw new Error(`hya binary not found at ${hyaBin}; run \`cargo build -p hya-backend --bin hya\` or set HYA_BIN`)
    }
    const root = await mkdtemp(join(tmpdir(), "hya-tui-web-"))
    const { child, backend } = await startBackend(root)
    await use(backend)
    if (child.exitCode === null) {
      const exited = new Promise((resolve) => child.once("exit", resolve))
      child.kill("SIGTERM")
      const timer = setTimeout(() => child.kill("SIGKILL"), 3_000)
      await exited
      clearTimeout(timer)
    }
    await rm(root, { recursive: true, force: true })
  },
  // Capture the final screen here, while `backend` is still running. The base
  // fixture tears down after the backend, so its capture would show the TUI's
  // reconnect error instead of the state under test.
  tui: async ({ tui, backend: _backend }, use, testInfo) => {
    let last: Tui | undefined
    await use(async (command, options) => (last = await tui(command, options)))
    if (last) await last.attach(testInfo, "final-screen").catch(() => {})
  },
})

export { expect } from "./harness"

/** argv that runs packages/hya-tui against `backend`. */
export function hyaTui(backend: Backend): string[] {
  return ["bun", join(repoRoot, "packages/hya-tui/src/main.ts"), "--server", backend.url, "--dir", backend.dir]
}
