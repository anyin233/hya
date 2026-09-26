import { afterEach, expect, test } from "bun:test"
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { mkdir, realpath } from "node:fs/promises"
import { BackendError, connectOrStart, databasePaths, defaultDatabase, findRunningServer, initialSessionId, parseDiscovery, probeHealth, resolveHyaBinary, startDaemon, type DaemonInfo } from "../src/launch"

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

// --- The backend daemon (ADR-0023): `hya serve start --json` starts or finds it.

const daemonJson = (pid: number, started: boolean, url = "http://127.0.0.1:4321") =>
  JSON.stringify({ url, pid, version: "0.41.0", startedAt: 1_700_000_000_000, db: "/s/sessions.db", log: "/s/sessions.db.server.log", started })

test("starts the daemon with `hya serve start --json --db <db>` in the directory and reads what it reports", async () => {
  const { bin, dir } = await fakeHya(`echo "note: noise" >&2\necho '${daemonJson(4242, true)}'`)
  const info = await startDaemon({ bin, directory: dir, db: join(dir, "s.db") })
  expect(info).toEqual({ url: "http://127.0.0.1:4321", pid: 4242, version: "0.41.0", startedAt: 1_700_000_000_000, db: "/s/sessions.db", started: true } satisfies DaemonInfo)
  expect((await readFile(join(dir, "argv"), "utf8")).trim()).toBe(`serve start --json --db ${join(dir, "s.db")}`)
  expect(await Bun.file(join(dir, "cwd")).text()).toContain(dir.split("/").at(-1)!)
})

test("a failed daemon start is an error carrying its exit code and output", async () => {
  const { bin, dir } = await fakeHya(`echo "Error: the hya server daemon did not answer within 60 s" >&2\nexit 1`)
  const failure = await startDaemon({ bin, directory: dir, db: join(dir, "s.db") }).then(() => undefined, (error: unknown) => error)
  expect(failure).toBeInstanceOf(BackendError)
  expect((failure as BackendError).message).toContain("hya serve start exited with code 1")
  expect((failure as BackendError).detail).toContain("did not answer within 60 s")
  expect((failure as BackendError).exitCode).toBe(1)
})

test("output that is not the daemon JSON is an error, not a guess", async () => {
  const { bin, dir } = await fakeHya(`echo "started hya server pid 1"`)
  const failure = await startDaemon({ bin, directory: dir, db: join(dir, "s.db") }).then(() => undefined, (error: unknown) => error)
  expect((failure as BackendError).message).toContain("unexpected output")
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

const daemon = (pid: number, started: boolean, url: string): DaemonInfo => ({ url, pid, version: "0.41.0", startedAt: 5, db: "/s/sessions.db", started })

test("attaches to the live daemon of the database instead of starting one", async () => {
  const db = "/s/sessions.db"
  let starts = 0
  const connection = await connectOrStart({ bin: "/b/hya", directory: "/w", db }, {
    readText: (path) => (path === databasePaths(db, "/w")!.discovery ? discovery(42, "http://127.0.0.1:9") : undefined),
    alive: () => true,
    fetcher: healthy,
    start: async () => { starts++; return daemon(1, true, "http://x") },
  })
  expect(connection).toEqual({ url: "http://127.0.0.1:9", pid: 42, db, version: "0.41.0", startedAt: 1, started: false })
  expect(starts).toBe(0)
})

test("with no live server (none published, stale, or shutting down) it starts the daemon", async () => {
  const db = "/s/sessions.db"
  const unavailable = (async () => new Response(JSON.stringify({ error: { code: "unavailable" } }), { status: 503 })) as unknown as typeof fetch
  for (const [readText, fetcher, alive] of [[() => undefined, refused, false], [() => discovery(42), refused, false], [() => discovery(42), unavailable, true]] as const) {
    const connection = await connectOrStart({ bin: "/b/hya", directory: "/w", db }, { readText, alive: () => alive, fetcher, start: async () => daemon(77, true, "http://127.0.0.1:7") })
    expect(connection).toEqual({ url: "http://127.0.0.1:7", pid: 77, db, version: "0.41.0", startedAt: 5, started: true })
  }
})

test("a daemon another client started in the meantime is attached, not reported as started", async () => {
  const connection = await connectOrStart({ bin: "/b/hya", directory: "/w", db: "/s/sessions.db" }, {
    readText: () => undefined, alive: () => false, fetcher: refused,
    start: async () => daemon(43, false, "http://127.0.0.1:8"),
  })
  expect(connection).toMatchObject({ pid: 43, started: false })
})

test("a failed start is reported, not retried", async () => {
  let starts = 0
  const error = await connectOrStart({ bin: "/b/hya", directory: "/w", db: "/s/sessions.db" }, {
    readText: () => undefined, alive: () => false, fetcher: refused,
    start: async () => { starts++; throw new BackendError("hya serve start exited with code 1", "database is held", 1) },
  }).catch((caught: unknown) => caught)
  expect(starts).toBe(1)
  expect((error as Error).message).toContain("code 1")
})
