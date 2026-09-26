/**
 * Losing the server (ADR-0023; docs/tui.md "When the server goes away"): the
 * backend is a daemon that outlives its clients, but it can be stopped
 * (`hya serve stop`/`restart`) or crash. A TUI that knows its database
 * (`--db`, or the one-command launch) then finds or starts the next server.
 *
 * `lost()` runs when a live stream fails or ends. A stream can drop for a
 * moment (a network blip, a busy server), so the server counts as gone only
 * when `GET /v1/health` fails `probes` times, `gapMs` apart (a server that is
 * shutting down answers `unavailable`, which fails the probe at once). Then
 * `reconnect()` runs the launch's attach-or-start (src/launch.ts
 * `connectOrStart`: the discovery file, else `hya serve start`) and
 * `switchTo()` moves the client to the result: new base URL, both streams
 * resubscribed, the open session's transcript, pending asks, and catalogs
 * reloaded. Calls while one runs join it (both streams fail together). A
 * failed attempt is reported; the streams keep retrying with their backoff,
 * and each retry calls `lost()` again.
 *
 * Why the server went away decides what happens (ADR-0023 amendment): a
 * stopping server sends `serverStopping {reason}` as the last frame of every
 * stream, which the controller passes to `stopping()` before the stream
 * ends.
 *
 * - `stop` (`hya serve stop`), `signal`, or an unknown reason: nothing is
 *   started. The TUI is *stopped*: it says `Backend stopped (hya serve
 *   stop) · /reconnect starts it again`, refuses prompts, and only looks
 *   (`find`, never start) for a server another client started, attaching
 *   to it when one appears.
 * - `restart` (`hya serve restart`): wait up to `restartWaitMs` (60 s) for
 *   the next server of the database (`find`) and attach to it; never start
 *   one. If none comes, the TUI is stopped.
 * - No reason (crash, kill -9, lost network): find or start, as above.
 *
 * `reconnectNow()` (`/reconnect`) finds or starts a server at once, from any
 * state.
 */

/** Where the client goes after losing its server. */
export interface ServerSwitch {
  url: string
  pid: number
  /** This TUI started it (else it was found: another client's, or already running). */
  started: boolean
  version?: string
  /** Unix ms when it started listening (for `/status`). */
  startedAt?: number
}

export interface ReconnectorOptions {
  /** The server the client uses now. */
  url(): string
  /** `GET <url>/v1/health` answers `ok: true` (src/launch.ts `probeHealth`). */
  probe(url: string): Promise<boolean>
  /** Find or start the database's server (src/launch.ts `connectOrStart`). */
  reconnect(): Promise<ServerSwitch>
  /** Only find the database's running server, never start one (src/launch.ts `findRunningServer`). Without it a stopped TUI waits for `/reconnect`. */
  find?(): Promise<ServerSwitch | undefined>
  /** Move the client to `next` and reload what it shows. */
  switchTo(next: ServerSwitch): Promise<void>
  status(text: string): void
  /** The TUI entered (`true`) or left (`false`) the stopped state. */
  onStopped?(stopped: boolean): void
  sleep?: (ms: number) => Promise<void>
  now?: () => number
  /** Failed probes before the server counts as gone (default 2). */
  probes?: number
  /** Wait between probes (default 500 ms). */
  gapMs?: number
  /** Longest wait for the next server after `restart` (default 60 s). */
  restartWaitMs?: number
  /** Interval of the lookups while waiting (default 500 ms). */
  pollMs?: number
}

/** The status notice after a switch. */
export function switchNotice(next: ServerSwitch): string {
  return next.started ? `Started a new server · pid ${next.pid}` : `Server moved · now pid ${next.pid}`
}

/** The status notice of a TUI whose server was stopped on purpose (`reason` from `serverStopping`). */
export function stoppedNotice(reason: string): string {
  const why = reason === "stop" ? "hya serve stop" : reason
  return `Backend stopped (${why}) · /reconnect starts it again`
}

export const restartWaitNotice = "Backend restarting (hya serve restart) · waiting for the new one…"
export const restartGoneNotice = "Backend did not come back after hya serve restart · /reconnect starts it again"

const bare = (url: string): string => url.replace(/\/+$/, "")

export function createReconnector({
  url, probe, reconnect, find, switchTo, status, onStopped,
  sleep = (ms) => Bun.sleep(ms), now = () => Date.now(),
  probes = 2, gapMs = 500, restartWaitMs = 60_000, pollMs = 500,
}: ReconnectorOptions) {
  let running: Promise<void> | undefined
  /** The last `serverStopping` reason, and the server that sent it. */
  let told: { url: string; reason: string } | undefined
  /** Stopped on purpose: never start a server until `/reconnect`. */
  let stopped = false
  /** `/reconnect` interrupts a restart wait. */
  let interrupted = false

  function setStopped(value: boolean): void {
    if (stopped === value) return
    stopped = value
    onStopped?.(value)
  }

  async function gone(target: string): Promise<boolean> {
    for (let attempt = 0; attempt < probes; attempt++) {
      if (attempt) await sleep(gapMs)
      if (await probe(target)) return false
    }
    return true
  }

  /** Attach to a server found without starting one. */
  async function attach(next: ServerSwitch): Promise<void> {
    await switchTo(next)
    told = undefined
    setStopped(false)
    status(switchNotice({ ...next, started: false }))
  }

  /** `restart`: wait for the next server of the database; stopped when none comes. */
  async function awaitRestart(): Promise<void> {
    status(restartWaitNotice)
    const deadline = now() + restartWaitMs
    while (!interrupted) {
      const next = await find?.().catch(() => undefined)
      if (next) return attach(next)
      if (now() >= deadline) break
      await sleep(pollMs)
    }
    if (interrupted) return
    setStopped(true)
    status(restartGoneNotice)
  }

  async function run(): Promise<void> {
    if (stopped) {
      // The streams keep retrying the dead URL; each retry only looks for a
      // server another client started. Quietly: the notice stays.
      const next = await find?.().catch(() => undefined)
      if (next) await attach(next)
      return
    }
    const lost = url()
    if (!(await gone(lost))) return
    const reason = told && told.url === bare(lost) ? told.reason : undefined
    told = undefined
    if (reason === "restart") return awaitRestart()
    if (reason !== undefined) {
      setStopped(true)
      status(stoppedNotice(reason))
      return
    }
    status("Server stopped · reconnecting…")
    let next: ServerSwitch
    try {
      next = await reconnect()
    } catch (error) {
      status(`Server lost: ${error instanceof Error ? error.message : String(error)} · retrying`)
      return
    }
    // The same server answered after all (slow, not gone): its streams recover by themselves.
    if (bare(next.url) === bare(lost) && !next.started) return
    await switchTo(next)
    status(switchNotice(next))
  }

  /** `/reconnect`: find or start the database's server now. */
  async function runNow(): Promise<void> {
    const wasStopped = stopped
    status("Reconnecting…")
    let next: ServerSwitch
    try {
      next = await reconnect()
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      status(`Reconnect failed: ${message} · /reconnect to try again`)
      return
    }
    told = undefined
    if (!wasStopped && !next.started && bare(next.url) === bare(url())) {
      status(`Connected · pid ${next.pid}`)
      return
    }
    await switchTo(next)
    setStopped(false)
    status(switchNotice(next))
  }

  return {
    /** A live stream failed or ended: check the server, and replace it when it is gone. */
    lost(): Promise<void> {
      running ??= run().finally(() => { running = undefined })
      return running
    },
    /** A `serverStopping {reason}` frame arrived from the server at `from`. */
    stopping(from: string, reason: string): void {
      told = { url: bare(from), reason }
    },
    /** `/reconnect`: interrupt a restart wait, then find or start a server. */
    async reconnectNow(): Promise<void> {
      interrupted = true
      await running?.catch(() => undefined)
      interrupted = false
      running = runNow().finally(() => { running = undefined })
      return running
    },
    /** Forget a stop reason and leave the stopped state (the TUI moved to another server by itself: `/disconnect-remote`). */
    reset(): void {
      told = undefined
      setStopped(false)
    },
    /** A check or switch is in progress. */
    busy: (): boolean => running !== undefined,
    /** Stopped on purpose (`hya serve stop`): prompts are refused and nothing is started until `/reconnect`. */
    stopped: (): boolean => stopped,
  }
}

export type Reconnector = ReturnType<typeof createReconnector>
