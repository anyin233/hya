/**
 * `/undo`, `/redo`, and `/fork` text and rows (docs/tui.md "Undo, redo, and
 * fork"; the calls are in app/revert.ts). Pure: no store, no client.
 */
import type { ForkSource, RevertedFile, SessionInfo, SessionRevert } from "../client"
import type { MessageView } from "./messages"
import type { PickerRow } from "./picker"

/** Id of the `/fork` picker's "fork at the latest message" row. */
export const forkHeadId = "__head__"

/** Id of the transcript's pending-revert line (state/messages.ts `transcriptViews`). */
export const revertIndicatorId = "revert-pending"

const plural = (count: number, word: string): string => `${count} ${word}${count === 1 ? "" : "s"}`

/** `path` relative to `workdir` when it is inside it, else as is. */
function shownPath(path: string, workdir: string): string {
  const root = workdir.replace(/\/+$/, "")
  return root && path.startsWith(`${root}/`) ? path.slice(root.length + 1) : path
}

/**
 * The status line after `/undo` (`Reverted · …`) or `/redo` (`Restored · …`):
 * counts of restored, deleted, and unchanged files, then every skipped or
 * failed file with its reason.
 */
export function revertSummary(files: readonly RevertedFile[] | undefined, options: { undone: boolean; workdir: string }): string {
  const head = options.undone ? "Restored" : "Reverted"
  const rows = files ?? []
  if (!rows.length) return `${head} · no file changes`
  const count = (action: string): number => rows.filter((file) => file.action === action).length
  const parts: string[] = []
  if (count("restored")) parts.push(`${plural(count("restored"), "file")} restored`)
  if (count("deleted")) parts.push(`${count("deleted")} deleted`)
  if (count("unchanged")) parts.push(`${count("unchanged")} unchanged`)
  for (const action of ["skipped", "failed"]) {
    for (const file of rows.filter((row) => row.action === action)) {
      parts.push(`${action} ${shownPath(file.path, options.workdir)}${file.reason ? ` (${file.reason})` : ""}`)
    }
  }
  return [head, ...parts].join(" · ")
}

/** The line at the transcript's end while a revert is pending. */
export function revertIndicator(revert: SessionRevert): string {
  const count = revert.hiddenMessages ?? 0
  const what = count > 0 ? plural(count, "message") : "messages"
  return `↶ ${what} reverted · /redo or Ctrl+X R restores ${count === 1 ? "it" : "them"} · the next prompt makes it permanent`
}

/** One line of a user view's text (whitespace collapsed). */
function promptLine(view: MessageView): string {
  return view.blocks.map((block) => (block.kind === "text" ? block.text : "")).join(" ").replace(/\s+/g, " ").trim()
}

/**
 * The `/fork` picker's rows: "Fork at the latest message" (highlighted),
 * then the transcript's user prompts newest first, tagged with their
 * number (`#1` is the oldest shown).
 */
export function forkRows(views: readonly MessageView[]): PickerRow[] {
  const prompts = views.filter((view) => view.role === "user" && !view.queued)
  const rows = prompts.map((view, index): PickerRow => ({
    id: view.id,
    label: promptLine(view) || "(empty prompt)",
    tag: `#${index + 1}`,
    detail: "copies the messages before it",
  })).reverse()
  return [{ id: forkHeadId, label: "Fork at the latest message", detail: "copies every message", current: true }, ...rows]
}

/** `forked from <title>` (sidebar, `/status`); `undefined` for a session that is not a fork. */
export function forkSourceText(source: ForkSource | undefined, sessions: readonly SessionInfo[]): string | undefined {
  if (!source?.session) return undefined
  const row = sessions.find((candidate) => candidate.id === source.session)
  return `forked from ${row?.title || source.session}`
}

/**
 * A fresh read of `current`'s row merged over it: `revert` and
 * `forkedFrom` are the fresh row's (absent there = gone: a redo or a
 * committed revert), everything else is merged.
 */
export function sessionRow(current: SessionInfo, row: SessionInfo): SessionInfo {
  const merged: SessionInfo = { ...current, ...row }
  if (!row.revert) delete merged.revert
  if (!row.forkedFrom) delete merged.forkedFrom
  return merged
}
