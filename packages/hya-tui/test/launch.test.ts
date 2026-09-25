import { afterEach, expect, test } from "bun:test"
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { BackendError, defaultDatabase, initialSessionId, parseReadyLine, resolveHyaBinary, startBackend } from "../src/launch"

const exists = (paths: string[]) => (path: string) => paths.includes(path)

test("binary lookup order: --hya, then HYA_BIN, then hya on PATH", () => {
  const which = (name: string) => (name === "hya" ? "/usr/bin/hya" : null)
  const all = exists(["/flag/hya", "/env/hya", "/usr/bin/hya"])
  expect(resolveHyaBinary({ flag: "/flag/hya", env: { HYA_BIN: "/env/hya" }, which, exists: all })).toEqual({ path: "/flag/hya", source: "--hya" })
  expect(resolveHyaBinary({ env: { HYA_BIN: "/env/hya" }, which, exists: all })).toEqual({ path: "/env/hya", source: "HYA_BIN" })
  expect(resolveHyaBinary({ env: {}, which, exists: all })).toEqual({ path: "/usr/bin/hya", source: "PATH" })
  // An empty HYA_BIN counts as unset.
  expect(resolveHyaBinary({ env: { HYA_BIN: "" }, which, exists: all })).toEqual({ path: "/usr/bin/hya", source: "PATH" })
})

test("a named binary that does not exist is an error, not a silent fallback", () => {
  const which = () => "/usr/bin/hya"
  const only = exists(["/usr/bin/hya"])
  expect(() => resolveHyaBinary({ flag: "/missing", env: {}, which, exists: only })).toThrow("--hya /missing does not exist")
  expect(() => resolveHyaBinary({ env: { HYA_BIN: "/missing" }, which, exists: only })).toThrow("HYA_BIN=/missing does not exist")
  const error = (() => {
    try {
      resolveHyaBinary({ env: {}, which: () => null, exists: only })
    } catch (caught) {
      return caught
    }
  })()
  expect(error).toBeInstanceOf(BackendError)
  expect(String((error as Error).message)).toContain("hya binary not found")
  expect(String((error as Error).message)).toContain("--server")
})

test("parses the hya serve readiness line", () => {
  expect(parseReadyLine("hya server listening on http://127.0.0.1:53211\n")).toBe("http://127.0.0.1:53211")
  expect(parseReadyLine("noise\nhya server listening on http://127.0.0.1:1/\nmore")).toBe("http://127.0.0.1:1/")
  expect(parseReadyLine("hya server listening on")).toBeUndefined()
  expect(parseReadyLine("starting…")).toBeUndefined()
})

test("the default database is the durable sessions.db that `hya sessions` reads", () => {
  expect(defaultDatabase({ XDG_STATE_HOME: "/state", HOME: "/home/u" })).toBe("/state/hya/sessions.db")
  expect(defaultDatabase({ HOME: "/home/u" })).toBe("/home/u/.local/state/hya/sessions.db")
  expect(defaultDatabase({ XDG_STATE_HOME: "", HOME: "/home/u" })).toBe("/home/u/.local/state/hya/sessions.db")
})

test("--continue picks the most recent top-level session of the directory; --session names one", () => {
  const sessions = [
    { id: "a", agent: "build", workdir: "/w", timeUpdated: "2026-09-25T10:00:00Z" },
    { id: "child", agent: "explore", workdir: "/w", parent: "a", timeUpdated: "2026-09-25T12:00:00Z" },
    { id: "b", agent: "build", workdir: "/w", timeUpdated: "2026-09-25T11:00:00Z" },
    { id: "elsewhere", agent: "build", workdir: "/other", timeUpdated: "2026-09-25T13:00:00Z" },
  ]
  expect(initialSessionId(sessions, { continue: true }, "/w")).toBe("b")
  expect(initialSessionId(sessions, { continue: false, session: "a" }, "/w")).toBe("a")
  expect(initialSessionId(sessions, { continue: false }, "/w")).toBeUndefined()
  expect(initialSessionId([], { continue: true }, "/w")).toBeUndefined()
})

const scratch: string[] = []
afterEach(async () => {
  for (const dir of scratch.splice(0)) await rm(dir, { recursive: true, force: true })
})

/** A stand-in `hya` script: records its argv, cwd, and pid, then behaves per `body`. */
async function fakeHya(body: string): Promise<{ bin: string; dir: string }> {
  const dir = await mkdtemp(join(tmpdir(), "hya-tui-launch-"))
  scratch.push(dir)
  const bin = join(dir, "hya")
  await writeFile(bin, `#!/bin/sh\necho "$@" > "${dir}/argv"\npwd > "${dir}/cwd"\necho $$ > "${dir}/pid"\n${body}\n`)
  await chmod(bin, 0o755)
  return { bin, dir }
}

const alive = (pid: number): boolean => {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

test("starts `hya serve --bind 127.0.0.1:0 --db <db>` in the directory, reads the URL, and stops it", async () => {
  const { bin, dir } = await fakeHya(`echo "starting" >&2\necho "hya server listening on http://127.0.0.1:4321"\nexec sleep 30`)
  const backend = await startBackend({ bin, directory: dir, db: join(dir, "s.db") })
  expect(backend.url).toBe("http://127.0.0.1:4321")
  expect((await readFile(join(dir, "argv"), "utf8")).trim()).toBe(`serve --bind 127.0.0.1:0 --db ${join(dir, "s.db")}`)
  expect(await Bun.file(join(dir, "cwd")).text()).toContain(dir.split("/").at(-1)!)
  expect(alive(backend.pid)).toBe(true)
  await backend.stop()
  expect(alive(backend.pid)).toBe(false)
  // Idempotent.
  await backend.stop()
})

test("a child that ignores SIGTERM is killed after the grace period", async () => {
  const { bin, dir } = await fakeHya(`trap '' TERM\necho "hya server listening on http://127.0.0.1:1"\nwhile true; do sleep 0.1; done`)
  const backend = await startBackend({ bin, directory: dir, db: ":memory:", graceMs: 300 })
  const started = Date.now()
  await backend.stop()
  expect(alive(backend.pid)).toBe(false)
  expect(Date.now() - started).toBeLessThan(3_000)
})

test("a server that exits before it is ready fails with its exit code and stderr tail", async () => {
  const { bin, dir } = await fakeHya(`echo "error: address in use" >&2\nexit 3`)
  const failure = await startBackend({ bin, directory: dir, db: ":memory:" }).then(() => undefined, (error: unknown) => error)
  expect(failure).toBeInstanceOf(BackendError)
  expect((failure as BackendError).message).toContain("exited with code 3 before it was ready")
  expect((failure as BackendError).detail).toContain("error: address in use")
})

test("a server that never prints the readiness line times out and is stopped", async () => {
  const { bin, dir } = await fakeHya(`echo "still booting" >&2\nexec sleep 30`)
  const failure = await startBackend({ bin, directory: dir, db: ":memory:", readyTimeoutMs: 300 }).then(() => undefined, (error: unknown) => error)
  expect((failure as BackendError).message).toContain("did not print its readiness line")
  expect((failure as BackendError).detail).toContain("still booting")
  const pid = Number((await readFile(join(dir, "pid"), "utf8")).trim())
  expect(alive(pid)).toBe(false)
})

test("keeps draining the child's output after it is ready (a full pipe would block the server)", async () => {
  const { bin, dir } = await fakeHya(`echo "hya server listening on http://127.0.0.1:1"\ni=0; while [ $i -lt 2000 ]; do echo "log line $i with padding padding padding padding" >&2; i=$((i+1)); done\necho done > "$(dirname "$0")/drained"\nexec sleep 30`)
  const backend = await startBackend({ bin, directory: dir, db: ":memory:" })
  const deadline = Date.now() + 5_000
  while (!(await Bun.file(join(dir, "drained")).exists()) && Date.now() < deadline) await Bun.sleep(20)
  expect(await Bun.file(join(dir, "drained")).exists()).toBe(true)
  expect(backend.outputTail()).toContain("log line 1999")
  await backend.stop()
})
