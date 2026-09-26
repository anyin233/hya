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
  /** Move the client to `next` and reload what it shows. */
  switchTo(next: ServerSwitch): Promise<void>
  status(text: string): void
  sleep?: (ms: number) => Promise<void>
  /** Failed probes before the server counts as gone (default 2). */
  probes?: number
  /** Wait between probes (default 500 ms). */
  gapMs?: number
}

/** The status notice after a switch. */
export function switchNotice(next: ServerSwitch): string {
  return next.started ? `Started a new server · pid ${next.pid}` : `Server moved · now pid ${next.pid}`
}

export function createReconnector({ url, probe, reconnect, switchTo, status, sleep = (ms) => Bun.sleep(ms), probes = 2, gapMs = 500 }: ReconnectorOptions) {
  let running: Promise<void> | undefined

  async function gone(target: string): Promise<boolean> {
    for (let attempt = 0; attempt < probes; attempt++) {
      if (attempt) await sleep(gapMs)
      if (await probe(target)) return false
    }
    return true
  }

  async function run(): Promise<void> {
    const lost = url()
    if (!(await gone(lost))) return
    status("Server stopped · reconnecting…")
    let next: ServerSwitch
    try {
      next = await reconnect()
    } catch (error) {
      status(`Server lost: ${error instanceof Error ? error.message : String(error)} · retrying`)
      return
    }
    // The same server answered after all (slow, not gone): its streams recover by themselves.
    if (next.url.replace(/\/+$/, "") === lost.replace(/\/+$/, "") && !next.started) return
    await switchTo(next)
    status(switchNotice(next))
  }

  return {
    /** A live stream failed or ended: check the server, and replace it when it is gone. */
    lost(): Promise<void> {
      running ??= run().finally(() => { running = undefined })
      return running
    },
    /** A check or switch is in progress. */
    busy: (): boolean => running !== undefined,
  }
}

export type Reconnector = ReturnType<typeof createReconnector>
