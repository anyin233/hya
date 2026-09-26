/**
 * The Diff view (`/diff`, docs/tui.md "Diff view"): pure state and keys over
 * `GET /v1/vcs/diff` (components/DiffView.tsx renders it; app/diff.ts owns
 * the call). The server returns one unified-diff text: `git diff HEAD` plus
 * every untracked file, each as its own `diff --git` section; `parseDiff`
 * splits it back into per-file rows.
 *
 * One file is open at a time; `n`/`p` (or `]`/`[`) switch files, `r`
 * reloads, Esc closes. Up/Down, PgUp/PgDn, Home/End, and the mouse wheel
 * scroll the open file's body — that is native `<scrollbox>` behavior
 * (components/DiffView.tsx, app/context.ts `DiffScroller`), so this module
 * only turns them into `scroll` outcomes for the component to act on.
 */
import type { KeyLike } from "../keys/bindings"
import { pickerWindow } from "./picker"
import { diffLines, type ToolLine } from "./tools"

export interface DiffFile {
  /** Repository-relative path when the server reports one; otherwise whatever the diff header carried. */
  path: string
  additions: number
  deletions: number
  lines: ToolLine[]
}

export interface DiffBusy {
  label: string
  startedAt: number
}

export interface DiffNotice {
  text: string
  tone: "info" | "ok" | "error"
}

export interface DiffViewState {
  files: readonly DiffFile[]
  /** Path of the open file; `undefined` when there are none. */
  current: string | undefined
  busy?: DiffBusy
  notice?: DiffNotice
}

export type ScrollAction = "line-up" | "line-down" | "page-up" | "page-down" | "top" | "bottom"

export type DiffViewOutcome =
  | { type: "none" }
  | { type: "update"; view: DiffViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "reload" }
  | { type: "scroll"; action: ScrollAction }

export interface DiffKeyRow {
  keys: string
  description: string
  hint?: string
}

export const diffKeyRows: readonly DiffKeyRow[] = [
  { keys: "Up / Down, mouse wheel", description: "Scroll the open file", hint: "↑↓ scroll" },
  { keys: "PgUp / PgDn", description: "Scroll a page" },
  { keys: "Home / End", description: "Jump to the top / bottom of the file" },
  { keys: "n / p, ] / [", description: "Next / previous file", hint: "n/p file" },
  { keys: "r", description: "Reload the diff", hint: "r reload" },
  { keys: "Esc", description: "Close the view" },
]

/**
 * Strip a `directory`-prefixed absolute path (an untracked file's `git diff
 * --no-index` header) down to a repository-relative one. Git's `a/`/`b/`
 * prefixes drop the leading `/` of an absolute path (`b//abs/path` would be
 * a doubled slash, so git writes `b/abs/path` instead), so the comparison
 * ignores a leading slash on both sides.
 */
function relativize(path: string, directory: string): string {
  const trimmed = path.replace(/\/+$/, "").replace(/^\/+/, "")
  const base = directory.replace(/\/+$/, "").replace(/^\/+/, "")
  if (base && trimmed.startsWith(`${base}/`)) return trimmed.slice(base.length + 1)
  return path
}

/** One `diff --git a/... b/...` section's display path: the `+++`/`---` side that is not `/dev/null`. */
function sectionPath(section: string, directory: string): string {
  const added = /^\+\+\+ (?:b\/)?(.+)$/m.exec(section)
  if (added && added[1] !== "/dev/null") return relativize(added[1]!.trim(), directory)
  const removed = /^--- (?:a\/)?(.+)$/m.exec(section)
  if (removed && removed[1] !== "/dev/null") return relativize(removed[1]!.trim(), directory)
  const header = /^diff --git a\/(.+?) b\/(.+)$/m.exec(section)
  if (header) return relativize((header[2] === "/dev/null" ? header[1] : header[2])!.trim(), directory)
  return "unknown"
}

/** Split the server's raw diff text into per-file rows, oldest section first (as the server emits them). */
export function parseDiff(raw: string, directory = ""): DiffFile[] {
  if (!raw.trim()) return []
  const sections = raw.split(/(?=^diff --git )/m).map((section) => section.replace(/^\n+/, "")).filter(Boolean)
  return sections.map((section) => {
    const lines = diffLines(section)
    return {
      path: sectionPath(section, directory),
      additions: lines.filter((line) => line.tone === "add").length,
      deletions: lines.filter((line) => line.tone === "remove").length,
      lines,
    }
  })
}

export function initialDiffView(files: readonly DiffFile[]): DiffViewState {
  return { files, current: files[0]?.path }
}

export function currentDiffFile(view: DiffViewState): DiffFile | undefined {
  return view.files.find((file) => file.path === view.current)
}

/** After a reload: keep the open file when it still exists, else the first (or none). */
export function settleDiffView(view: DiffViewState, files: readonly DiffFile[]): DiffViewState {
  const current = files.some((file) => file.path === view.current) ? view.current : files[0]?.path
  return { ...view, files, current }
}

function switchFile(view: DiffViewState, step: number): DiffViewState {
  if (!view.files.length) return view
  const at = Math.max(0, view.files.findIndex((file) => file.path === view.current))
  return { ...view, current: view.files[(at + step + view.files.length) % view.files.length]!.path }
}

/** One key while the Diff view is open. */
export function diffViewKey(view: DiffViewState, key: KeyLike): DiffViewOutcome {
  if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }
  if (key.name === "escape") return { type: "close" }
  if (key.name === "up") return { type: "scroll", action: "line-up" }
  if (key.name === "down") return { type: "scroll", action: "line-down" }
  if (key.name === "pageup") return { type: "scroll", action: "page-up" }
  if (key.name === "pagedown") return { type: "scroll", action: "page-down" }
  if (key.name === "home") return { type: "scroll", action: "top" }
  if (key.name === "end") return { type: "scroll", action: "bottom" }
  if (key.ctrl || key.meta) return { type: "none" }
  if (key.sequence === "n" || key.sequence === "]") return { type: "update", view: switchFile(view, 1) }
  if (key.sequence === "p" || key.sequence === "[") return { type: "update", view: switchFile(view, -1) }
  if (key.sequence === "r") return { type: "reload" }
  return { type: "none" }
}

/** One file list row: path, `+N -M`, marked when open (fits `width`). */
export function fileLine(file: DiffFile, open: boolean, width: number): string {
  const marker = open ? "▸ " : "  "
  const counts = `+${file.additions} -${file.deletions}`
  const line = `${marker}${file.path}`
  const gap = Math.max(1, width - Bun.stringWidth(line) - Bun.stringWidth(counts))
  return `${line}${" ".repeat(gap)}${counts}`
}

export interface DiffFileWindow {
  start: number
  end: number
  /** Files hidden above the window (0 when the window starts at the top). */
  moreAbove: number
  /** Files hidden below the window (0 when the window reaches the last file). */
  moreBelow: number
}

/**
 * The slice of `files` to show around the open file, plus how many files are
 * hidden on each side (a `N more` indicator; components/DiffView.tsx renders
 * it). Many changed files would otherwise overflow the file list with no way
 * to see the rest.
 */
export function diffFileWindow(files: readonly DiffFile[], current: string | undefined, visible: number): DiffFileWindow {
  const at = Math.max(0, files.findIndex((file) => file.path === current))
  const { start, end } = pickerWindow(files.length, at, Math.max(1, visible))
  return { start, end, moreAbove: start, moreBelow: files.length - end }
}

/** The footer hint. */
export function diffViewHint(view: DiffViewState): string {
  if (view.busy) return `${view.busy.label}… · Esc cancels`
  if (view.notice) return view.notice.text
  return "↑↓ scroll · n/p file · r reload · Esc close"
}
