/**
 * One-command launch (docs/tui.md "Start it"): without `--server` the TUI
 * starts its own backend and owns its lifetime.
 *
 * - Binary: `--hya <path>`, else `HYA_BIN`, else `hya` on PATH. A path named
 *   by the flag or the variable must exist; it never silently falls through.
 * - Process: `hya serve --bind 127.0.0.1:0 --db <db>` in `--dir`, with the
 *   TUI's environment. stdout and stderr are piped (the TUI owns the
 *   terminal) and drained for the whole run; the last lines are kept for
 *   error reports.
 * - Ready: the first `hya server listening on <url>` line (the readiness
 *   contract in docs/cli.md) gives the base URL. A child that exits first, or
 *   prints nothing within `readyTimeoutMs`, is an error carrying the output
 *   tail.
 * - Stop: SIGTERM, then SIGKILL after `graceMs`; resolves once the child has
 *   exited. The child stays in the TUI's process group, so a hangup of the
 *   terminal reaches it too.
 *
 * One writer per database (ADR-0022, `connectOrStart`): a server holds an
 * exclusive lock on `<db>.lock` and publishes `<db>.server.json`
 * (`{url, pid, version, startedAt}`) once listening. Before starting one the
 * TUI reads that file; a live pid whose `GET /v1/health` answers `ok` is
 * attached to (and never stopped by this TUI). Otherwise it starts
 * `hya serve`; if that exits 75 (the database is held: another launcher won
 * the race, or its server is still starting) the TUI waits for the holder's
 * discovery file and attaches, or fails with a clear error.
 */
import { existsSync, mkdirSync, readFileSync, realpathSync } from "node:fs"
import { basename, dirname, join, resolve } from "node:path"
import type { SessionInfo } from "./client"
import { newestTopLevelSession } from "./state/projects"

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

const readyPattern = /hya server listening on (\S+)/

/** The base URL from `hya serve`'s readiness line, if `text` contains it. */
export function parseReadyLine(text: string): string | undefined {
  return readyPattern.exec(text)?.[1]
}

/** `$XDG_STATE_HOME/hya/sessions.db`, else `$HOME/.local/state/hya/sessions.db` — the store `hya sessions` reads. */
export function defaultDatabase(env: Record<string, string | undefined>): string {
  const base = env.XDG_STATE_HOME || (env.HOME ? join(env.HOME, ".local/state") : ".local/state")
  return join(base, "hya", "sessions.db")
}

/**
 * The session to open at start: `--session <id>` as given, `--continue` the
 * most recently updated top-level session of `projectId` — the Project
 * ensured for `--dir` (state/projects.ts `newestTopLevelSession`; list
 * order breaks ties) — else none (a new session is created by the first
 * prompt). Without a Project (`--remote`) there is nothing to continue.
 */
export function initialSessionId(sessions: readonly SessionInfo[], startup: { continue: boolean; session?: string }, projectId: string | undefined): string | undefined {
  if (startup.session) return startup.session
  if (!startup.continue || !projectId) return undefined
  return newestTopLevelSession(sessions, projectId)?.id
}

export interface BackendOptions {
  bin: string
  directory: string
  db: string
  env?: Record<string, string | undefined>
  /** Longest wait for the readiness line (default 60 s: a first run may build catalogs). */
  readyTimeoutMs?: number
  /** Wait after SIGTERM before SIGKILL (default 6 s: serve drains turns for up to 5 s). */
  graceMs?: number
}

export interface Backend {
  url: string
  pid: number
  bin: string
  db: string
  /** Last lines of the child's stdout and stderr. */
  outputTail(): string
  /** Stop the child and wait for it to exit (idempotent). */
  stop(): Promise<void>
  readonly exited: Promise<number | null>
}

const tailLimit = 8192

export async function startBackend(options: BackendOptions): Promise<Backend> {
  const { bin, directory, db, readyTimeoutMs = 60_000, graceMs = 6_000 } = options
  if (db && db !== ":memory:" && !db.startsWith("file:")) {
    try {
      mkdirSync(dirname(db), { recursive: true })
    } catch {
      // hya serve reports an unusable path itself.
    }
  }
  let child: Bun.Subprocess<"ignore", "pipe", "pipe">
  try {
    child = Bun.spawn([bin, "serve", "--bind", "127.0.0.1:0", "--db", db], {
      cwd: directory,
      env: options.env ?? process.env,
      stdin: "ignore",
      stdout: "pipe",
      stderr: "pipe",
    })
  } catch (error) {
    throw new BackendError(`could not start ${bin}: ${error instanceof Error ? error.message : String(error)}`)
  }
  let tail = ""
  let stdoutText = ""
  let onReady: ((url: string) => void) | undefined
  const append = (text: string): void => {
    tail = (tail + text).slice(-tailLimit)
  }
  const drain = async (stream: ReadableStream<Uint8Array>, stdout: boolean): Promise<void> => {
    const decoder = new TextDecoder()
    const reader = stream.getReader()
    try {
      for (;;) {
        const { value: chunk, done } = await reader.read()
        if (done) break
        const text = decoder.decode(chunk, { stream: true })
        append(text)
        if (stdout && onReady) {
          stdoutText = (stdoutText + text).slice(-tailLimit)
          const url = parseReadyLine(stdoutText)
          if (url) onReady(url)
        }
      }
    } catch {
      // The child is gone; its exit is reported through `exited`.
    }
  }
  void drain(child.stdout, true)
  void drain(child.stderr, false)
  const exited = child.exited.then((code) => code, () => null)
  let stopping: Promise<void> | undefined
  const stop = (): Promise<void> => {
    stopping ??= (async () => {
      if (child.exitCode !== null || child.signalCode !== null) return
      child.kill("SIGTERM")
      const timer = setTimeout(() => child.kill("SIGKILL"), graceMs)
      await exited
      clearTimeout(timer)
    })()
    return stopping
  }
  const outputTail = (): string => tail.split("\n").slice(-40).join("\n").trim()
  const url = await new Promise<string>((resolve, reject) => {
    const timer = setTimeout(() => {
      onReady = undefined
      void stop().then(() => reject(new BackendError(`hya serve did not print its readiness line within ${Math.round(readyTimeoutMs / 1000)} s`, outputTail())))
    }, readyTimeoutMs)
    onReady = (found) => {
      onReady = undefined
      clearTimeout(timer)
      resolve(found)
    }
    void exited.then(async (code) => {
      if (!onReady) return
      onReady = undefined
      clearTimeout(timer)
      // Let the drains take the last output before reporting it.
      await Bun.sleep(20)
      reject(new BackendError(`hya serve exited with code ${code ?? "(signal)"} before it was ready`, outputTail(), code ?? undefined))
    })
  })
  return { url, pid: child.pid, bin, db, outputTail, stop, exited }
}

/** `hya serve`'s exit status when another process holds the database (docs/cli.md "`hya serve`"). */
export const exitDatabaseInUse = 75

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
  start?: (options: BackendOptions) => Promise<Backend>
  sleep?: (ms: number) => Promise<void>
  now?: () => number
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

/** How the TUI reached its server without `--server`. */
export type Connection =
  | { kind: "attached"; url: string; pid: number; db: string; version: string }
  | { kind: "started"; backend: Backend }

/**
 * Attach to the running server of `options.db`, else start `hya serve`. A
 * start that exits 75 (the database is held) waits up to `attachWaitMs`
 * (default 20 s) for the holder's server, then fails.
 */
export async function connectOrStart(options: BackendOptions & { attachWaitMs?: number }, deps: LaunchDeps = {}): Promise<Connection> {
  const { db, directory, attachWaitMs = 20_000 } = options
  const attached = (found: DiscoveryInfo): Connection => ({ kind: "attached", url: found.url, pid: found.pid, db, version: found.version })
  const running = await findRunningServer(db, directory, deps)
  if (running) return attached(running)
  try {
    return { kind: "started", backend: await (deps.start ?? startBackend)(options) }
  } catch (error) {
    if (!(error instanceof BackendError) || error.exitCode !== exitDatabaseInUse) throw error
    const sleep = deps.sleep ?? ((ms: number) => Bun.sleep(ms))
    const now = deps.now ?? Date.now
    const deadline = now() + attachWaitMs
    for (;;) {
      const found = await findRunningServer(db, directory, deps)
      if (found) return attached(found)
      if (now() >= deadline) {
        throw new BackendError(
          `database ${db} is in use by another hya process that serves no reachable server (waited ${Math.round(attachWaitMs / 1000)} s); stop that process, or pass another --db`,
          error.detail,
          error.exitCode,
        )
      }
      await sleep(250)
    }
  }
}
