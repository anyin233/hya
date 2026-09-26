/**
 * Sessions on connect and exit (docs/tui.md "Sessions on start and exit"): a
 * TUI started without `--session`/`--continue` creates a session as soon as
 * it connects, so it is ready to type into. That session (and every one
 * `/new` makes) is created `ephemeral`: the backend daemon deletes it once it
 * is still unused and no client watches it (ADR-0023 amendment "the daemon
 * drops unused sessions"). So this client never deletes anything itself:
 * leaving an unused session (another one opened, or the TUI exiting in any
 * way, a kill included) just stops watching it.
 *
 * How the TUI exits decides what happens to a session that is used
 * (`leave`, ADR-0023 amendment "how a client leaves its session"):
 *
 * | Exit | Mode | Used session |
 * | --- | --- | --- |
 * | Ctrl+C twice, `/exit`, `/quit` | `archive` | archived (`PATCH {archived:true}`; the root when a subagent's view is open) |
 * | Ctrl+D, `/to-background` (terminal only) | `background` | left as is, keeps running on the daemon |
 * | SIGHUP (WebUI tab closed), SIGTERM, SIGINT, a crash | `signal` | left as is, keeps running on the daemon |
 *
 * An unused session is never archived (archiving would keep it for good):
 * one this client created and never sent anything in is left without a
 * request, so the exit never waits for it; any other is archived only when
 * the server says it is not `ephemeral`. Archiving is only a flag: a running
 * turn keeps running and finishes on the daemon.
 */
import type { SessionInfo } from "../client"

export interface SessionKeeperClient {
  getSession(id: string): Promise<SessionInfo>
  /** `PATCH {archived: true}` on a root session. */
  archiveSession(id: string): Promise<void>
}

/** How the TUI exits (the table above). */
export type ExitMode = "archive" | "background" | "signal"

/** `unused`: left for the daemon to drop; `archived`; `kept`: left as is (running, or an archive failed). */
export type LeaveOutcome = "unused" | "archived" | "kept"

/** Most parent hops walked to find a subagent session's root. */
const maxDepth = 32

export interface SessionKeeperOptions {
  client: SessionKeeperClient
}

export function createSessionKeeper({ client }: SessionKeeperOptions) {
  /** Sessions this client created and has not sent anything in yet. */
  const fresh = new Set<string>()

  /** The root session of the session `first` describes (itself unless it is a subagent's). */
  async function rootOf(first: SessionInfo): Promise<string> {
    let current = first
    for (let hop = 0; hop < maxDepth; hop++) {
      const parent = current.parent
      if (!parent || parent === current.id) return current.id
      current = await client.getSession(parent)
    }
    return current.id
  }

  return {
    /** This client created `id` (on connect, or `/new`) as an ephemeral session. */
    created(id: string): void { fresh.add(id) },
    /** The user sent something in `id`: the daemon keeps it from now on. */
    used(id: string): void { fresh.delete(id) },
    isFresh: (id: string): boolean => fresh.has(id),
    /**
     * Exiting with `id` open: an unused session is left to the daemon (no
     * request for one this client created); else archive its root on a
     * graceful exit; `background` and `signal` leave it running. Never
     * throws: a failed read or archive keeps the session as is.
     */
    async leave(id: string, mode: ExitMode): Promise<LeaveOutcome> {
      if (fresh.has(id)) return "unused"
      if (mode !== "archive") return "kept"
      try {
        const session = await client.getSession(id)
        if (session.ephemeral) return "unused"
        await client.archiveSession(await rootOf(session))
        return "archived"
      } catch {
        return "kept"
      }
    },
  }
}

export type SessionKeeper = ReturnType<typeof createSessionKeeper>
