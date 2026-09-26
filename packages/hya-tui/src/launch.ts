/**
 * One-command launch (docs/tui.md "Start it"): without `--server` the TUI
 * connects to the backend daemon of its database, starting one if none runs
 * (ADR-0023). The daemon outlives the TUI; nothing here ever stops it.
 *
 * - Binary: `--hya <path>`, else `HYA_BIN`, else `hya` on PATH. A path named
 *   by the flag or the variable must exist; it never silently falls through.
 * - Discovery (ADR-0022, `findRunningServer`): a server holds an exclusive
 *   lock on `<db>.lock` and publishes `<db>.server.json`
 *   (`{url, pid, version, startedAt}`) once listening. The TUI attaches when
 *   that pid is alive and `GET /v1/health` answers `ok` (a server that is
 *   shutting down answers `unavailable`).
 * - Start (`startDaemon`): otherwise it runs `hya serve start --json --db
 *   <db>` in `--dir`, which starts `hya serve` detached (own session, output
 *   to `<db>.server.log`), waits until it answers, and prints
 *   `{url, pid, version, startedAt, db, log, started}`. The lock arbitrates
 *   races between clients; `started` is false when another client's daemon
 *   won.
 *
 * The same attach-or-start runs again when the TUI loses its server
 * (app/reconnect.ts).
 */
import { existsSync, readFileSync, realpathSync } from "node:fs"
import { basename, dirname, join, resolve } from "node:path"
import type { SessionInfo } from "./client"

/** A backend start failure; `detail` is the child's output tail (may be empty), `exitCode` the child's status when it exited. */
export class BackendError extends Error {
  constructor(message: string, readonly detail = "", readonly exitCode?: number) {
    super(message)
    this.name = "BackendError"
  }
}

export interface BinaryLookup {
  /** `--hya` */
  flag?: string
  env: Record<string, string | undefined>
  which?: (name: string) => string | null
  exists?: (path: string) => boolean
}

export interface ResolvedBinary {
  path: string
  source: "--hya" | "HYA_BIN" | "PATH"
}

export function resolveHyaBinary({ flag, env, which = (name) => Bun.which(name), exists = existsSync }: BinaryLookup): ResolvedBinary {
  if (flag !== undefined) {
    if (!exists(flag)) throw new BackendError(`hya binary not found: --hya ${flag} does not exist`)
    return { path: flag, source: "--hya" }
  }
  const variable = env.HYA_BIN
  if (variable) {
    if (!exists(variable)) throw new BackendError(`hya binary not found: HYA_BIN=${variable} does not exist`)
    return { path: variable, source: "HYA_BIN" }
  }
  const found = which("hya")
  if (found) return { path: found, source: "PATH" }
  throw new BackendError("hya binary not found: pass --hya <path>, set HYA_BIN, or put hya on PATH (or connect to a running backend with --server <url>)")
}

/** `$XDG_STATE_HOME/hya/sessions.db`, else `$HOME/.local/state/hya/sessions.db` — the store `hya sessions` reads. */
export function defaultDatabase(env: Record<string, string | undefined>): string {
  const base = env.XDG_STATE_HOME || (env.HOME ? join(env.HOME, ".local/state") : ".local/state")
  return join(base, "hya", "sessions.db")
}

/**
 * The session to open at start: `--session <id>` as given, `--continue` the
 * most recently updated top-level session of `directory` that is not
 * archived (list order breaks ties), else none.
 */
export function initialSessionId(sessions: readonly SessionInfo[], startup: { continue: boolean; session?: string }, directory: string): string | undefined {
  if (startup.session) return startup.session
  if (!startup.continue) return undefined
  const time = (session: SessionInfo): number => Date.parse(session.timeUpdated ?? "") || 0
  const candidates = sessions.filter((session) => !session.parent && !session.archived && (!session.workdir || session.workdir === directory))
  return candidates.reduce<SessionInfo | undefined>((best, session) => (!best || time(session) > time(best) ? session : best), undefined)?.id
}

export interface DaemonOptions {
  bin: string
  /** Working directory of `hya serve start` (and so of a daemon it starts). */
  directory: string
  db: string
  env?: Record<string, string | undefined>
}

/** What `hya serve start --json` reports: the running daemon, and whether this call started it. */
export interface DaemonInfo {
  url: string
  pid: number
  version: string
  /** Unix time in ms when the server started listening. */
  startedAt: number
  db: string
  started: boolean
}

const tailLimit = 8192

/** Run `hya serve start --json --db <db>` in `directory`; resolves to the daemon it reports. */
export async function startDaemon(options: DaemonOptions): Promise<DaemonInfo> {
  const { bin, directory, db } = options
  let child: Bun.Subprocess<"ignore", "pipe", "pipe">
  try {
    child = Bun.spawn([bin, "serve", "start", "--json", "--db", db], {
      cwd: directory,
      env: options.env ?? process.env,
      stdin: "ignore",
      stdout: "pipe",
      stderr: "pipe",
    })
  } catch (error) {
    throw new BackendError(`could not run ${bin} serve start: ${error instanceof Error ? error.message : String(error)}`)
  }
  const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited])
  const detail = `${stderr}${stdout}`.slice(-tailLimit).trim()
  if (code !== 0) throw new BackendError(`hya serve start exited with code ${code}`, detail, code)
  const info = parseDaemonInfo(stdout.trim().split("\n").at(-1) ?? "")
  if (!info) throw new BackendError("hya serve start printed unexpected output (is --hya an older hya?)", detail)
  return info
}

function parseDaemonInfo(line: string): DaemonInfo | undefined {
  const found = parseDiscovery(line)
  if (!found) return undefined
  const value = JSON.parse(line) as Record<string, unknown>
  return { ...found, db: typeof value.db === "string" ? value.db : "", started: value.started === true }
}

/** The discovery file a running server publishes next to its database (`<db>.server.json`). */
export interface DiscoveryInfo {
  url: string
  pid: number
  version: string
  /** Unix time in ms when the server started listening. */
  startedAt: number
}

/**
 * `<db>.lock` and `<db>.server.json` for `db` as the started server sees it:
 * relative to `directory` (its working directory), with the directory
 * canonicalized the way `hya` does. `undefined` for stores that are not
 * locked (in-memory, SQLite URIs).
 */
export function databasePaths(db: string, directory: string): { lock: string; discovery: string } | undefined {
  if (!db || db === ":memory:" || db.startsWith("file:") || db.startsWith("sqlite:")) return undefined
  const path = resolve(directory, db)
  let dir = dirname(path)
  try {
    dir = realpathSync(dir)
  } catch {
    // Not created yet: no server can be running on it.
  }
  const name = basename(path)
  return { lock: join(dir, `${name}.lock`), discovery: join(dir, `${name}.server.json`) }
}

/** A well-formed discovery file, else `undefined`. */
export function parseDiscovery(text: string): DiscoveryInfo | undefined {
  let value: unknown
  try {
    value = JSON.parse(text)
  } catch {
    return undefined
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined
  const { url, pid, version, startedAt } = value as Record<string, unknown>
  if (typeof url !== "string" || !/^https?:\/\//.test(url)) return undefined
  if (typeof pid !== "number" || !Number.isInteger(pid) || pid <= 0) return undefined
  return { url, pid, version: typeof version === "string" ? version : "", startedAt: typeof startedAt === "number" ? startedAt : 0 }
}

/** Whether `GET <url>/v1/health` answers `{ ok: true }` within `timeoutMs`. */
export async function probeHealth(url: string, fetcher: typeof fetch = fetch, timeoutMs = 2_000): Promise<boolean> {
  try {
    const response = await fetcher(`${url.replace(/\/+$/, "")}/v1/health`, { signal: AbortSignal.timeout(timeoutMs) })
    if (!response.ok) return false
    const body = (await response.json()) as { ok?: unknown }
    return body?.ok === true
  } catch {
    return false
  }
}

function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    // EPERM: it exists but belongs to someone else.
    return (error as NodeJS.ErrnoException).code === "EPERM"
  }
}

function readTextFile(path: string): string | undefined {
  try {
    return readFileSync(path, "utf8")
  } catch {
    return undefined
  }
}

/** Injectable effects of the attach-or-start decision (tests replace them). */
export interface LaunchDeps {
  readText?: (path: string) => string | undefined
  alive?: (pid: number) => boolean
  fetcher?: typeof fetch
  start?: (options: DaemonOptions) => Promise<DaemonInfo>
}

/**
 * The live server of `db`: its discovery file parses, the pid is alive, and
 * the URL's health probe answers. A stale file (crashed server) is ignored;
 * the next server to take the lock overwrites it.
 */
export async function findRunningServer(db: string, directory: string, deps: LaunchDeps = {}): Promise<DiscoveryInfo | undefined> {
  const paths = databasePaths(db, directory)
  if (!paths) return undefined
  const text = (deps.readText ?? readTextFile)(paths.discovery)
  const found = text === undefined ? undefined : parseDiscovery(text)
  if (!found || !(deps.alive ?? processAlive)(found.pid)) return undefined
  return (await probeHealth(found.url, deps.fetcher)) ? found : undefined
}

/** The server the TUI uses: the database's daemon, found or started. */
export interface Connection {
  url: string
  pid: number
  db: string
  version: string
  /** Unix time in ms when the server started listening. */
  startedAt: number
  /** This call started it (else it was running, or another client's start won). */
  started: boolean
}

/**
 * Attach to the running server of `options.db`, else start the daemon
 * (`hya serve start`, which also resolves races between clients).
 */
export async function connectOrStart(options: DaemonOptions, deps: LaunchDeps = {}): Promise<Connection> {
  const { db, directory } = options
  const running = await findRunningServer(db, directory, deps)
  if (running) return { url: running.url, pid: running.pid, db, version: running.version, startedAt: running.startedAt, started: false }
  const info = await (deps.start ?? startDaemon)(options)
  return { url: info.url, pid: info.pid, db, version: info.version, startedAt: info.startedAt, started: info.started }
}
