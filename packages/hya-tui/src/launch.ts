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
 */
import { existsSync, mkdirSync } from "node:fs"
import { dirname, join } from "node:path"
import type { SessionInfo } from "./client"

/** A backend start failure; `detail` is the child's output tail (may be empty). */
export class BackendError extends Error {
  constructor(message: string, readonly detail = "") {
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
 * most recently updated top-level session of `directory` (list order breaks
 * ties), else none (a new session is created by the first prompt).
 */
export function initialSessionId(sessions: readonly SessionInfo[], startup: { continue: boolean; session?: string }, directory: string): string | undefined {
  if (startup.session) return startup.session
  if (!startup.continue) return undefined
  const time = (session: SessionInfo): number => Date.parse(session.timeUpdated ?? "") || 0
  const candidates = sessions.filter((session) => !session.parent && (!session.workdir || session.workdir === directory))
  return candidates.reduce<SessionInfo | undefined>((best, session) => (!best || time(session) > time(best) ? session : best), undefined)?.id
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
      reject(new BackendError(`hya serve exited with code ${code ?? "(signal)"} before it was ready`, outputTail()))
    })
  })
  return { url, pid: child.pid, bin, db, outputTail, stop, exited }
}
