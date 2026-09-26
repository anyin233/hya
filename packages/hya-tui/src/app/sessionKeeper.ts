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
 */
import type { MessageInfo, SessionInfo } from "../client"

export interface SessionKeeperClient {
  getSession(id: string): Promise<SessionInfo>
  listMessages(id: string): Promise<MessageInfo[]>
  deleteSession(id: string): Promise<void>
}

export type DropOutcome = "deleted" | "kept" | "notOurs"

export interface SessionKeeperOptions {
  client: SessionKeeperClient
  /** This client has a turn running or prompts queued in the session. */
  localBusy?: (id: string) => boolean
}

export function createSessionKeeper({ client, localBusy = () => false }: SessionKeeperOptions) {
  /** Sessions this client created and has not used (nor decided on) yet. */
  const fresh = new Set<string>()

  return {
    /** This client created `id` (on connect, or `/new`). */
    created(id: string): void { fresh.add(id) },
    /** The user sent something in `id`: it is never dropped. */
    used(id: string): void { fresh.delete(id) },
    isFresh: (id: string): boolean => fresh.has(id),
    /** Leaving `id`: delete it when this client created it and it is still empty. */
    async dropIfEmpty(id: string): Promise<DropOutcome> {
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
    },
  }
}

export type SessionKeeper = ReturnType<typeof createSessionKeeper>
