/**
 * `/undo`, `/redo`, and `/fork` (docs/tui.md "Undo, redo, and fork"; the
 * text and rows are state/revert.ts).
 *
 * - `/undo` sends `RevertSession {}` (the last visible prompt; again = one
 *   further back), re-reads the transcript, and puts the reverted prompt
 *   (`SessionInfo.revert.text`) in the input — only when the input is empty
 *   or still holds the text the previous `/undo` put there untouched, so a
 *   draft the user typed is never replaced.
 * - `/redo` (only while `SessionInfo.revert` is set) sends
 *   `RevertSession {undo: true}`, re-reads, and empties the input when it
 *   still holds exactly the reverted prompt.
 * - `/fork` opens the picker (head first, then the prompts newest first);
 *   Enter sends `ForkSession`, switches to the new session, and puts
 *   `promptText` in an empty input.
 *
 * The server refuses a revert while a turn runs (`409 session_busy`); the
 * refusal is shown on the status line.
 */
import { HttpError, type HyaClient, type SessionInfo } from "../client"
import { transcriptViews } from "../state/messages"
import type { PickerSpec } from "../state/picker"
import { truncate } from "../state/format"
import { forkHeadId, forkRows, revertSummary } from "../state/revert"
import type { AppStore } from "../state/store"

/** The composer's input (app/controller.ts `ComposerAccess`). */
export interface RevertComposer {
  text(): string
  setText(text: string): void
}

export interface RevertControllerOptions {
  store: AppStore
  client: Pick<HyaClient, "revertSession" | "forkSession" | "listMessages">
  /** The mounted composer, if any. */
  composer(): RevertComposer | undefined
  openSession(id: string): Promise<void>
  /** Re-read the catalogs (the sidebar lists the new fork). */
  refresh(): Promise<void>
  openPicker(spec: PickerSpec): void
}

/** Status when `/undo`, `/redo`, or `/fork` runs in a subagent's read-only view. */
export const revertReadOnlyStatus = "Read-only: this is a subagent's session · Esc returns to the parent"

const busyText = "a turn is running · wait for it to finish or press Esc to cancel it"

export function createRevertController({ store, client, composer, openSession, refresh, openPicker }: RevertControllerOptions) {
  /** The text the last `/undo` or `/fork` put in the input (replaceable while untouched). */
  let prefilled: string | undefined
  const status = (text: string): void => store.setStatus(text)

  /** Put `text` in the input when it is empty or still holds the last prefill; `"filled"`, `"kept"` (the user's text stays), or `"none"`. */
  function prefill(text: string | undefined): "filled" | "kept" | "none" {
    const input = composer()
    if (!text || !input) return "none"
    const current = input.text()
    if (current.trim() && current !== prefilled) return "kept"
    input.setText(text)
    prefilled = text
    return "filled"
  }

  /** The open session, when a revert may run in it; sets the status and returns `undefined` otherwise. */
  function target(verb: string): SessionInfo | undefined {
    const selected = store.state.selected
    if (!selected) {
      status(`Nothing to ${verb}: no session is open`)
      return undefined
    }
    if (selected.parent) {
      status(revertReadOnlyStatus)
      return undefined
    }
    return selected
  }

  async function reload(sessionId: string): Promise<void> {
    store.setMessages(sessionId, await client.listMessages(sessionId))
    store.followTranscript()
  }

  async function undo(): Promise<void> {
    const selected = target("undo")
    if (!selected) return
    let result: Awaited<ReturnType<typeof client.revertSession>>
    try {
      result = await client.revertSession(selected.id, {})
    } catch (error) {
      if (error instanceof HttpError && error.status === 409) status(`Undo refused: ${busyText}`)
      else if (error instanceof HttpError && error.status === 400) status(`Nothing to undo: ${error.detail}`)
      else status(`Undo failed: ${error instanceof HttpError ? error.detail : String(error)}`)
      return
    }
    store.applyRevert(result.session)
    await reload(selected.id)
    const outcome = prefill(result.session.revert?.text)
    const note = outcome === "filled" ? " · the prompt is back in the input" : outcome === "kept" ? " · the input kept your text" : ""
    status(`${revertSummary(result.files, { undone: false, workdir: selected.workdir })}${note}`)
  }

  async function redo(): Promise<void> {
    const selected = target("redo")
    if (!selected) return
    const pending = selected.revert
    if (!pending) {
      status("Nothing to redo · /redo works after /undo, until the next prompt")
      return
    }
    let result: Awaited<ReturnType<typeof client.revertSession>>
    try {
      result = await client.revertSession(selected.id, { undo: true })
    } catch (error) {
      if (error instanceof HttpError && error.status === 409) status(`Redo refused: ${busyText}`)
      else if (error instanceof HttpError && error.status === 400) {
        // Committed meanwhile (a prompt from another client): the revert is gone.
        const { revert: _gone, ...rest } = selected
        store.applyRevert(rest)
        status(`Nothing to redo: ${error.detail}`)
      } else status(`Redo failed: ${error instanceof HttpError ? error.detail : String(error)}`)
      return
    }
    store.applyRevert(result.session)
    await reload(selected.id)
    const input = composer()
    if (input && pending.text && input.text() === pending.text) input.setText("")
    prefilled = undefined
    status(revertSummary(result.files, { undone: true, workdir: selected.workdir }))
  }

  function fork(): void {
    const selected = store.state.selected
    if (!selected) {
      status("Nothing to fork: no session is open")
      return
    }
    openPicker({
      title: "Fork · the new session ends before the picked prompt",
      rows: forkRows(transcriptViews(store.state)),
      hint: "Enter forks and switches to the new session · Esc closes · type to filter",
      onSelect: async (row) => {
        const messageId = row.id === forkHeadId ? undefined : row.id
        let result: Awaited<ReturnType<typeof client.forkSession>>
        try {
          result = await client.forkSession(selected.id, messageId)
        } catch (error) {
          status(`Fork failed: ${error instanceof HttpError ? error.detail : String(error)}`)
          return
        }
        await refresh()
        await openSession(result.session.id)
        const outcome = messageId ? prefill(result.promptText) : "none"
        const note = outcome === "filled" ? " · the prompt is in the input" : outcome === "kept" ? " · the input kept your text" : ""
        // The new session's header names it (the backend titles a fork `forked from <source>`).
        status(`${messageId ? `Forked before “${truncate(row.label, 40)}”` : "Forked at the latest message"}${note}`)
      },
    })
  }

  return { undo, redo, fork }
}
