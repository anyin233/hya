/**
 * Resume a session (`--resume [id]`, `/resume [id]`, an archived row of the
 * `/sessions` picker; docs/tui.md "Sessions on start and exit"): clear its
 * archived state (`PATCH {archived:false}`, idempotent) and open it. Without
 * an id, a picker offers the directory's root sessions, archived ones
 * included and marked, newest first (state/catalog.ts `resumeRows`).
 *
 * The terminal TUI and WebUI tabs of one database share its daemon, so this
 * is also how one resumes the other's sessions; a WebUI tab cannot pass
 * flags, so `/resume` is its way in.
 */
import type { SessionInfo } from "../client"
import { resumeRows } from "../state/catalog"
import type { PickerSpec } from "../state/picker"
import type { AppStore } from "../state/store"

export interface ResumerClient {
  listSessions(options: { includeArchived?: boolean }): Promise<SessionInfo[]>
  setArchived(id: string, archived: boolean): Promise<SessionInfo>
}

export interface ResumerOptions {
  store: AppStore
  client: ResumerClient
  /** `--dir`: whose sessions the picker offers. */
  directory: string
  openSession(id: string): Promise<void>
  openPicker(spec: PickerSpec): void
  /** Re-read the session list, so the unarchived session is back in the sidebar. */
  refresh?: () => Promise<void>
}

export function createResumer({ store, client, directory, openSession, openPicker, refresh }: ResumerOptions) {
  /** Unarchive `id`, then open it. */
  async function reopen(id: string): Promise<void> {
    const info = await client.setArchived(id, false)
    await refresh?.().catch(() => undefined)
    await openSession(id)
    store.setStatus(`Resumed ${info.title || id}`)
  }

  return {
    /**
     * With an id, unarchive and open it. Without, open the picker; `onCancel`
     * runs when it is closed without a choice (the startup picker opens a new
     * session then).
     */
    async resume(id?: string, onCancel?: () => void): Promise<void> {
      if (id) return reopen(id)
      const sessions = await client.listSessions({ includeArchived: true })
      const rows = resumeRows(sessions, directory, store.state.selected?.id)
      if (!rows.length) {
        store.setStatus("No session to resume in this directory")
        onCancel?.()
        return
      }
      openPicker({
        title: "Resume a session · archived ones included",
        rows,
        hint: "Enter resumes (and unarchives) · Esc closes · type to filter",
        onSelect: (row) => reopen(row.id),
        ...(onCancel ? { onCancel } : {}),
      })
    },
  }
}

export type Resumer = ReturnType<typeof createResumer>
