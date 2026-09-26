import { afterEach, expect, test } from "bun:test"
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { mkdir, realpath } from "node:fs/promises"
import { BackendError, connectOrStart, databasePaths, defaultDatabase, exitDatabaseInUse, findRunningServer, initialSessionId, parseDiscovery, parseReadyLine, probeHealth, resolveHyaBinary, startBackend, type Backend } from "../src/launch"

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

test("--continue picks the most recent top-level session of the ensured Project; --session names one", () => {
  const sessions = [
    { id: "a", agent: "build", workdir: "/w", projectId: "prj_w", timeUpdated: "2026-09-25T10:00:00Z" },
    { id: "child", agent: "explore", workdir: "/w", projectId: "prj_w", parent: "a", timeUpdated: "2026-09-25T12:00:00Z" },
    // Another workdir inside the same Project counts: the Project, not the workdir, is matched.
    { id: "b", agent: "build", workdir: "/w/sub", projectId: "prj_w", timeUpdated: "2026-09-25T11:00:00Z" },
    { id: "elsewhere", agent: "build", workdir: "/other", projectId: "prj_o", timeUpdated: "2026-09-25T13:00:00Z" },
    { id: "temp", agent: "build", workdir: "/cache/x", kind: "SESSION_KIND_TEMPORARY" as const, timeUpdated: "2026-09-25T14:00:00Z" },
  ]
  expect(initialSessionId(sessions, { continue: true }, "prj_w")).toBe("b")
  expect(initialSessionId(sessions, { continue: false, session: "a" }, "prj_w")).toBe("a")
  expect(initialSessionId(sessions, { continue: false }, "prj_w")).toBeUndefined()
  expect(initialSessionId([], { continue: true }, "prj_w")).toBeUndefined()
  // No Project (--remote, or the ensure failed): nothing to continue.
  expect(initialSessionId(sessions, { continue: true }, undefined)).toBeUndefined()
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

// --- One writer per database (ADR-0022): attach to a running server or start one.

test("the lock and discovery files sit next to the database, resolved from --dir with a canonical directory", async () => {
  const dir = await mkdtemp(join(tmpdir(), "hya-tui-paths-"))
  scratch.push(dir)
  await mkdir(join(dir, "state"))
  const real = await realpath(dir)
  expect(databasePaths(join(dir, "state/s.db"), "/elsewhere")).toEqual({ lock: join(real, "state/s.db.lock"), discovery: join(real, "state/s.db.server.json") })
  // A relative --db is relative to --dir (the started server's working directory).
  expect(databasePaths("state/s.db", dir)?.discovery).toBe(join(real, "state/s.db.server.json"))
  for (const db of ["", ":memory:", "file:x.db?mode=memory"]) expect(databasePaths(db, dir)).toBeUndefined()
})

test("parses the discovery file and rejects anything malformed", () => {
  expect(parseDiscovery('{"url":"http://127.0.0.1:5","pid":42,"version":"0.41.0","startedAt":1700}')).toEqual({ url: "http://127.0.0.1:5", pid: 42, version: "0.41.0", startedAt: 1700 })
  for (const text of ["", "not json", "{}", '{"url":"http://x","pid":"42"}', '{"url":"ftp://x","pid":1}', '{"url":"http://x","pid":0}', "[]", "null"]) {
    expect(parseDiscovery(text)).toBeUndefined()
  }
})

test("the health probe accepts only GET /v1/health answering ok: true", async () => {
  const seen: string[] = []
  const fetcher = (async (input: string | URL | Request) => {
    const url = String(input)
    seen.push(url)
    if (url.startsWith("http://good")) return Response.json({ ok: true, version: "1" })
    if (url.startsWith("http://sick")) return Response.json({ ok: false })
    if (url.startsWith("http://err")) return new Response("no", { status: 500 })
    throw new TypeError("connection refused")
  }) as typeof fetch
  expect(await probeHealth("http://good:1", fetcher)).toBe(true)
  expect(seen).toEqual(["http://good:1/v1/health"])
  expect(await probeHealth("http://good:1/", fetcher)).toBe(true)
  expect(await probeHealth("http://sick:1", fetcher)).toBe(false)
  expect(await probeHealth("http://err:1", fetcher)).toBe(false)
  expect(await probeHealth("http://down:1", fetcher)).toBe(false)
})

const discovery = (pid: number, url = "http://127.0.0.1:5") => JSON.stringify({ url, pid, version: "0.41.0", startedAt: 1 })
const healthy = (async () => Response.json({ ok: true })) as unknown as typeof fetch
const refused = (async () => { throw new TypeError("refused") }) as unknown as typeof fetch

test("a running server is found only with a readable discovery file, a live pid, and a healthy URL", async () => {
  const files = new Map<string, string>()
  const deps = (alive: boolean, fetcher: typeof fetch) => ({ readText: (path: string) => files.get(path), alive: () => alive, fetcher })
  const db = "/s/sessions.db"
  expect(await findRunningServer(db, "/w", deps(true, healthy))).toBeUndefined()
  files.set(databasePaths(db, "/w")!.discovery, discovery(42))
  expect(await findRunningServer(db, "/w", deps(true, healthy))).toMatchObject({ pid: 42, url: "http://127.0.0.1:5" })
  // Stale: the process is gone, or the URL does not answer.
  expect(await findRunningServer(db, "/w", deps(false, healthy))).toBeUndefined()
  expect(await findRunningServer(db, "/w", deps(true, refused))).toBeUndefined()
})

function fakeBackend(url: string): Backend & { stopped: boolean } {
  const backend = { url, pid: 77, bin: "/b/hya", db: "/s/sessions.db", stopped: false, outputTail: () => "", exited: new Promise<number | null>(() => {}), stop: async () => { backend.stopped = true } }
  return backend
}

test("attaches to the live server of the database instead of starting one", async () => {
  const db = "/s/sessions.db"
  let starts = 0
  const connection = await connectOrStart({ bin: "/b/hya", directory: "/w", db }, {
    readText: (path) => (path === databasePaths(db, "/w")!.discovery ? discovery(42, "http://127.0.0.1:9") : undefined),
    alive: () => true,
    fetcher: healthy,
    start: async () => { starts++; return fakeBackend("http://x") },
  })
  expect(connection).toEqual({ kind: "attached", url: "http://127.0.0.1:9", pid: 42, db, version: "0.41.0" })
  expect(starts).toBe(0)
})

test("with no live server (none published, or stale) it starts hya serve", async () => {
  const db = "/s/sessions.db"
  for (const readText of [() => undefined, () => discovery(42)]) {
    const started = fakeBackend("http://127.0.0.1:7")
    const connection = await connectOrStart({ bin: "/b/hya", directory: "/w", db }, { readText, alive: () => false, fetcher: refused, start: async () => started })
    expect(connection).toEqual({ kind: "started", backend: started })
  }
})

test("losing the start race (hya serve exits 75) attaches to the winner once it publishes", async () => {
  const db = "/s/sessions.db"
  let published = false
  let starts = 0
  const connection = await connectOrStart({ bin: "/b/hya", directory: "/w", db }, {
    readText: () => (published ? discovery(43, "http://127.0.0.1:8") : undefined),
    alive: () => true,
    fetcher: healthy,
    sleep: async () => { published = true },
    start: async () => { starts++; throw new BackendError("hya serve exited with code 75 before it was ready", "hya serve: database is already in use", exitDatabaseInUse) },
  })
  expect(starts).toBe(1)
  expect(connection).toMatchObject({ kind: "attached", pid: 43, url: "http://127.0.0.1:8" })
})

test("a database held by a process that never answers is a clear error", async () => {
  const db = "/s/sessions.db"
  let now = 0
  const error = await connectOrStart({ bin: "/b/hya", directory: "/w", db, attachWaitMs: 1_000 }, {
    readText: () => undefined,
    alive: () => true,
    fetcher: refused,
    now: () => now,
    sleep: async (ms) => { now += ms },
    start: async () => { throw new BackendError("hya serve exited with code 75 before it was ready", "hya serve: database /s/sessions.db is already in use by pid 9", exitDatabaseInUse) },
  }).catch((caught: unknown) => caught)
  expect(error).toBeInstanceOf(BackendError)
  expect((error as BackendError).message).toContain("database /s/sessions.db is in use by another hya process")
  expect((error as BackendError).message).toContain("--db")
  expect((error as BackendError).detail).toContain("already in use by pid 9")
})

test("other start failures are not retried", async () => {
  let starts = 0
  const error = await connectOrStart({ bin: "/b/hya", directory: "/w", db: "/s/sessions.db" }, {
    readText: () => undefined, alive: () => false, fetcher: refused,
    start: async () => { starts++; throw new BackendError("hya serve exited with code 2 before it was ready", "", 2) },
  }).catch((caught: unknown) => caught)
  expect(starts).toBe(1)
  expect((error as Error).message).toContain("code 2")
})

test("a server that exits before it is ready reports its exit code on the error", async () => {
  const { bin, dir } = await fakeHya(`echo "hya serve: database x is already in use" >&2\nexit 75`)
  const error = await startBackend({ bin, directory: dir, db: join(dir, "s.db") }).catch((caught: unknown) => caught)
  expect(error).toBeInstanceOf(BackendError)
  expect((error as BackendError).exitCode).toBe(75)
})
