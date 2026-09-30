import { afterEach, expect, test } from "bun:test"
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { parseArguments } from "../src/cli"
import { reloadArguments, reloadExitCode, reloadFileEnv, reloadStateEnv, requestReload, supervisorEnv, takeReloadState, takeSupervision } from "../src/reload"
import { supervise, type SupervisedChild } from "../src/supervisor"

const scratch: string[] = []
afterEach(() => { for (const dir of scratch.splice(0)) rmSync(dir, { recursive: true, force: true }) })
function tempFile(): string {
  const dir = mkdtempSync(join(tmpdir(), "hya-tui-reload-test-"))
  scratch.push(dir)
  return join(dir, "reload.json")
}

test("reload arguments reopen the open session and drop every other startup choice", () => {
  const argv = ["--server", "http://127.0.0.1:1/", "--dir", "/w", "--db", "/s.db", "--resume", "hysec_old", "--web-tab"]
  const next = reloadArguments(argv, { session: "hysec_open", server: "http://127.0.0.1:2/" })
  expect(next).toEqual(["--server", "http://127.0.0.1:2/", "--dir", "/w", "--db", "/s.db", "--web-tab", "--session", "hysec_open"])
  // The result is a valid command line (--session no longer clashes with --continue/--resume).
  expect(parseArguments(next, "/")?.session).toBe("hysec_open")

  // `--resume` without an id does not swallow the flag after it; `-c`, `-s` go too.
  expect(reloadArguments(["--resume", "--web-tab", "-c"], {})).toEqual(["--web-tab"])
  expect(reloadArguments(["-s", "hysec_a", "--dir", "/w"], { session: "hysec_b" })).toEqual(["--dir", "/w", "--session", "hysec_b"])
  // No --server before: none is added (the database's daemon is found again).
  expect(reloadArguments(["--db", "/s.db", "--continue"], { server: "http://127.0.0.1:9/" })).toEqual(["--db", "/s.db"])
})

test("the app takes its reload environment once, so no child (shell, editor, nested TUI) inherits it", () => {
  const env: Record<string, string | undefined> = {
    [reloadFileEnv]: "/tmp/r.json",
    [supervisorEnv]: "4242",
    [reloadStateEnv]: JSON.stringify({ draft: { text: "half typed", cursor: 4 } }),
  }
  expect(takeSupervision(env)).toEqual({ file: "/tmp/r.json", parent: 4242 })
  expect(takeReloadState(env)).toEqual({ draft: { text: "half typed", cursor: 4 } })
  expect(env).toEqual({})
  expect(takeSupervision(env)).toBeUndefined()
  expect(takeReloadState(env)).toBeUndefined()
  // Garbage is no state (the app starts plainly).
  expect(takeReloadState({ [reloadStateEnv]: "{" })).toBeUndefined()
})

/** Fake apps: each exits with the code its function returns (the function may write a reload request first). */
function fakeSpawner(exits: Array<(command: string[], env: Record<string, string | undefined>) => number>) {
  const runs: Array<{ command: string[]; env: Record<string, string | undefined> }> = []
  const spawn = (command: string[], env: Record<string, string | undefined>): SupervisedChild => {
    runs.push({ command, env })
    const exit = exits.shift()
    if (!exit) throw new Error("unexpected spawn")
    return { exited: Promise.resolve().then(() => exit(command, env)), kill: () => undefined }
  }
  return { spawn, runs }
}

test("the supervisor starts the app again from disk when it asks to reload, with the new arguments and draft", async () => {
  const file = tempFile()
  const fake = fakeSpawner([
    (_command, env) => {
      requestReload(env[reloadFileEnv]!, { argv: ["--session", "hysec_1"], draft: { text: "hi", cursor: 2 } })
      return reloadExitCode
    },
    () => 0,
  ])
  const code = await supervise({ bun: "/bin/bun", entry: "/tui/src/main.ts", argv: ["--continue"], env: { KEEP: "1" }, file, spawn: fake.spawn })
  expect(code).toBe(0)
  expect(fake.runs.map((run) => run.command)).toEqual([
    ["/bin/bun", "/tui/src/main.ts", "--continue"],
    ["/bin/bun", "/tui/src/main.ts", "--session", "hysec_1"],
  ])
  expect(fake.runs[0]!.env[reloadFileEnv]).toBe(file)
  expect(fake.runs[0]!.env[reloadStateEnv]).toBeUndefined()
  expect(fake.runs[1]!.env.KEEP).toBe("1")
  expect(JSON.parse(fake.runs[1]!.env[reloadStateEnv]!)).toEqual({ draft: { text: "hi", cursor: 2 } })
  // The request is consumed.
  expect(existsSync(file)).toBe(false)
})

test("the supervisor exits with the app's code; the reload code without a request is just a code", async () => {
  const file = tempFile()
  expect(await supervise({ bun: "b", entry: "e", argv: [], env: {}, file, spawn: fakeSpawner([() => 130]).spawn })).toBe(130)
  expect(await supervise({ bun: "b", entry: "e", argv: [], env: {}, file, spawn: fakeSpawner([() => reloadExitCode]).spawn })).toBe(reloadExitCode)
  // A malformed request is not followed either.
  writeFileSync(file, "{not json")
  expect(await supervise({ bun: "b", entry: "e", argv: [], env: {}, file, spawn: fakeSpawner([() => reloadExitCode]).spawn })).toBe(reloadExitCode)
})

test("a signal goes to the running app, and after it no reload starts another one", async () => {
  const file = tempFile()
  let deliver!: (signal: NodeJS.Signals) => void
  let release!: () => void
  const gate = new Promise<void>((resolve) => (release = resolve))
  const kills: string[] = []
  let spawns = 0
  const spawn = (_command: string[], env: Record<string, string | undefined>): SupervisedChild => {
    spawns++
    return {
      exited: gate.then(() => {
        requestReload(env[reloadFileEnv]!, { argv: [] })
        return reloadExitCode
      }),
      kill: (signal) => { kills.push(signal) },
    }
  }
  // The first app starts before `supervise` returns its promise, so the signal has a target.
  const done = supervise({ bun: "b", entry: "e", argv: [], env: {}, file, spawn, onSignal: (handler) => { deliver = handler } })
  deliver("SIGHUP")
  expect(kills).toEqual(["SIGHUP"])
  release()
  expect(await done).toBe(129)
  expect(spawns).toBe(1)
})
