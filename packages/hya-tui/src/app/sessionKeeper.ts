/**
 * Sessions on connect and exit (docs/tui.md "Sessions"): a TUI started
 * without `--session`/`--continue` creates a session as soon as it connects,
 * so it is ready to type into. Empty sessions must not pile up, so this
 * client deletes a session it created and never used when it leaves it:
 * `/new`, `/open`, `/sessions`, a `/fork` switch, or the TUI exiting (a WebUI
 * tab closing too).
 *
 * Only sessions this client created are candidates, each is decided once,
 * and emptiness is re-checked on the server right before the delete: no
 * messages, not busy, no title (a user's `/rename` or the automatic title
 * after a first prompt), not a subagent's. A read failure keeps the session.
 * The check and the delete are two requests; a prompt another client sends
 * into the empty session between them is lost with it (accepted: the window
 * is one round trip, and only for a session its creator is leaving).
 *
 * How the TUI exits decides what happens to a session that is kept
 * (`leave`, docs/tui.md "Sessions on start and exit"; ADR-0023 amendment):
 *
 * | Exit | Mode | Kept session |
 * | --- | --- | --- |
 * | Ctrl+C twice, `/exit`, `/quit` | `archive` | archived (`PATCH {archived:true}`; the root when a subagent's view is open) |
 * | Ctrl+D, `/to-background` (terminal only) | `background` | left as is, keeps running on the daemon |
 * | SIGHUP (WebUI tab closed), SIGTERM, SIGINT, a crash | `signal` | left as is, keeps running on the daemon |
 *
 * An empty session this client created is dropped on every exit. Archiving
 * is only a flag: a running turn keeps running and finishes on the daemon.
 */
import type { MessageInfo, SessionInfo } from "../client"

export interface SessionKeeperClient {
  getSession(id: string): Promise<SessionInfo>
  listMessages(id: string): Promise<MessageInfo[]>
  deleteSession(id: string): Promise<void>
  /** `PATCH {archived: true}` on a root session (needed by `leave(…, "archive")` only). */
  archiveSession?(id: string): Promise<void>
}

export type DropOutcome = "deleted" | "kept" | "notOurs"

/** How the TUI exits (the table above). */
export type ExitMode = "archive" | "background" | "signal"

export type LeaveOutcome = "deleted" | "archived" | "kept"

/** Most parent hops walked to find a subagent session's root. */
const maxDepth = 32

export interface SessionKeeperOptions {
  client: SessionKeeperClient
  /** This client has a turn running or prompts queued in the session. */
  localBusy?: (id: string) => boolean
}

export function createSessionKeeper({ client, localBusy = () => false }: SessionKeeperOptions) {
  /** Sessions this client created and has not used (nor decided on) yet. */
  const fresh = new Set<string>()

  async function dropIfEmpty(id: string): Promise<DropOutcome> {
    if (!fresh.delete(id)) return "notOurs"
    if (localBusy(id)) return "kept"
    try {
      const session = await client.getSession(id)
      if (session.busy || session.title || session.parent) return "kept"
      if ((await client.listMessages(id)).length) return "kept"
      await client.deleteSession(id)
      return "deleted"
    } catch {
      return "kept"
    }
  }

  /** The root session of `id` (itself unless it is a subagent's). */
  async function rootOf(id: string): Promise<string> {
    let current = id
    for (let hop = 0; hop < maxDepth; hop++) {
      const parent = (await client.getSession(current)).parent
      if (!parent || parent === current) return current
      current = parent
    }
    return current
  }

  return {
    /** This client created `id` (on connect, or `/new`). */
    created(id: string): void { fresh.add(id) },
    /** The user sent something in `id`: it is never dropped. */
    used(id: string): void { fresh.delete(id) },
    isFresh: (id: string): boolean => fresh.has(id),
    /** Leaving `id`: delete it when this client created it and it is still empty. */
    dropIfEmpty,
    /**
     * Exiting with `id` open: drop it when empty (any mode), else archive its
     * root on a graceful exit; `background` and `signal` leave it running.
     * Never throws: a failed archive keeps the session as is.
     */
    async leave(id: string, mode: ExitMode): Promise<LeaveOutcome> {
      if ((await dropIfEmpty(id)) === "deleted") return "deleted"
      if (mode !== "archive" || !client.archiveSession) return "kept"
      try {
        await client.archiveSession(await rootOf(id))
        return "archived"
      } catch {
        return "kept"
      }
    },
  }
}

export type SessionKeeper = ReturnType<typeof createSessionKeeper>
