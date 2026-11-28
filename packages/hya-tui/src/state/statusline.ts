/**
 * The status line (ADR-0036, docs/tui-config.md "Status line"): one row
 * below the input naming the model, the permission mode, and the context
 * occupancy at any width. Pure: segment text from the store, then the fit to
 * a width. Every value comes from fields the Context pane already reads; the
 * line appends no events.
 */
import { contextUsage, formatTokens, sessionTokens, truncate, truncateStart } from "./format"
import { effectiveMode, modeDisplay } from "./modes"
import type { AppState } from "./store"
import type { StatusSegment } from "../config/presets"
import { extensionManager } from "../extensions/manager"
import { paneLeaves, visiblePaneRoot, type VisiblePaneOptions } from "./panes"

export type SegmentTone = "muted" | "fg" | "accent" | "warning" | "error"

export interface StatusPart {
  id: StatusSegment
  text: string
  tone: SegmentTone
}

/** Context percent from which `ctx` warns (warning color) and alarms (error color). */
export const contextWarnPercent = 80
export const contextAlarmPercent = 95
/** Cells of the `ctx` occupancy bar. */
export const contextBarCells = 5
/** The `cwd` segment keeps this many columns of a directory's tail. */
const directoryColumns = 24

/** Dropped first to last when the line is too narrow; the others always stay. */
export const elisionOrder: readonly StatusSegment[] = ["session", "tokens", "git", "cwd", "agent"]

/** `━━──── 42%`-style bar: filled cells for the percentage, rounded, at least one above 0%. */
export function contextBar(percent: number, cells = contextBarCells): string {
  const clamped = Math.max(0, Math.min(100, percent))
  const filled = clamped === 0 ? 0 : Math.max(1, Math.round((clamped / 100) * cells))
  return "━".repeat(Math.min(cells, filled)) + "─".repeat(Math.max(0, cells - filled))
}

const home = (path: string, env: Record<string, string | undefined>): string => {
  const dir = env.HOME?.replace(/\/+$/, "")
  return dir && (path === dir || path.startsWith(`${dir}/`)) ? `~${path.slice(dir.length)}` : path
}

/** One segment's text and tone, or `undefined` when it has nothing to say (no session, no usage, connected). */
export function statusPart(state: AppState, id: StatusSegment, env: Record<string, string | undefined> = process.env): StatusPart | undefined {
  const session = state.selected
  switch (id) {
    case "model": {
      const model = session?.model?.modelId
      if (!model) return session ? { id, text: "default model", tone: "fg" } : undefined
      const effort = session.effectiveEffort
      return { id, text: effort && effort !== "default" ? `${model} · ${effort}` : model, tone: "fg" }
    }
    case "mode": {
      const mode = modeDisplay(effectiveMode(state), state.permissionModes)
      return { id, text: mode.text, tone: mode.tone === "error" ? "error" : mode.tone === "accent" ? "accent" : "muted" }
    }
    case "ctx": {
      // Hidden, like the Context pane's row, until a round reported usage against a known limit.
      const usage = session ? contextUsage(state) : undefined
      if (!usage) return undefined
      const percent = usage.percent
      const tone = percent >= contextAlarmPercent ? "error" : percent >= contextWarnPercent ? "warning" : "muted"
      return { id, text: `ctx ${contextBar(percent)} ${percent}%`, tone }
    }
    case "connection":
      if (state.backendStopped) return { id, text: "backend stopped", tone: "error" }
      return state.connected || !state.ready ? undefined : { id, text: "reconnecting", tone: "warning" }
    case "agent":
      return session?.agent ? { id, text: session.agent, tone: "muted" } : undefined
    case "cwd": {
      const project = state.projects.find((row) => row.id === state.activeProjectId)
      if (project?.name) return { id, text: project.name, tone: "muted" }
      const directory = session?.workdir
      return directory ? { id, text: truncateStart(home(directory, env), directoryColumns), tone: "muted" } : undefined
    }
    case "git":
      return state.gitBranch ? { id, text: `⎇ ${state.gitBranch}`, tone: "muted" } : undefined
    case "tokens": {
      const tokens = session ? sessionTokens(session.usage) : undefined
      return tokens === undefined ? undefined : { id, text: `${formatTokens(tokens)} tok`, tone: "muted" }
    }
    case "session":
      return session ? { id, text: session.title || session.id, tone: "muted" } : undefined
  }
}

/** Separator between left segments. */
export const separator = " · "
const width = (text: string): number => Bun.stringWidth(text)

export interface FittedStatusLine {
  left: StatusPart[]
  /** `session`, right-aligned, when it fits. */
  right?: StatusPart
}

/**
 * Fit the configured segments into `columns`: `session` goes to the right
 * end, the rest left in configured order; while the line is too wide, drop
 * `elisionOrder` segments (session first, agent last), then cut the model
 * name. `model`, `mode`, `ctx`, and `connection` are never dropped.
 */
export function fitStatusLine(parts: readonly StatusPart[], columns: number): FittedStatusLine {
  let left = parts.filter((part) => part.id !== "session")
  let right = parts.find((part) => part.id === "session")
  const leftWidth = (items: readonly StatusPart[]): number => items.reduce((sum, part, index) => sum + width(part.text) + (index ? separator.length : 0), 0)
  const total = (): number => leftWidth(left) + (right ? width(right.text) + 2 : 0)
  for (const id of elisionOrder) {
    if (total() <= columns) break
    if (id === "session") right = undefined
    else left = left.filter((part) => part.id !== id)
  }
  const over = total() - columns
  if (over > 0) {
    left = left.map((part) => part.id === "model" ? { ...part, text: truncate(part.text, Math.max(1, width(part.text) - over)) } : part)
  }
  return right ? { left, right } : { left }
}

/** The fitted line for the store at `columns` (inner width, padding excluded). */
export function statusLine(state: AppState, segments: readonly StatusSegment[], columns: number, env: Record<string, string | undefined> = process.env): FittedStatusLine {
  const parts = segments.flatMap((id) => statusPart(state, id, env) ?? [])
  return fitStatusLine(parts, columns)
}

/** Plain text of a fitted line (`session` right-aligned at `columns`), for tests and `/config`. */
export function statusLineText(line: FittedStatusLine, columns: number): string {
  const left = line.left.map((part) => part.text).join(separator)
  if (!line.right) return left
  return left + " ".repeat(Math.max(2, columns - width(left) - width(line.right.text))) + line.right.text
}

/** One string of the given segments for the input border (`composer.border_info`): no elision, cut to `columns`. */
export function borderInfoText(state: AppState, segments: readonly StatusSegment[], columns: number, env: Record<string, string | undefined> = process.env): string {
  const text = segments.flatMap((id) => statusPart(state, id, env) ?? []).map((part) => part.text).join(separator)
  return text ? truncate(` ${text} `, Math.max(0, columns)) : ""
}

/**
 * Whether the status line is drawn at `placement` (below the input, or as the
 * band right above a band input): the component is on, and no visible
 * Context pane already shows these facts (ADR-0036 "Deduplication"; the
 * workspace preset below 150 columns hides its sidebar, so the line returns).
 */
export function statusLineShown(
  state: AppState,
  columns: number,
  placement: "below" | "band" = "below",
  paneOptions: VisiblePaneOptions = extensionManager.visiblePaneOptions(),
): boolean {
  const settings = state.structure.statusline
  if (!settings.enabled) return false
  // A band needs the band input (`composer.style = "band"`); with a boxed input it goes below.
  const effective = settings.placement === "band" && state.structure.composer.style === "band" ? "band" : "below"
  if (effective !== placement) return false
  if (!state.structure.sidebar.enabled) return true
  return !paneLeaves(visiblePaneRoot(state.paneLayout.root, columns, state.sidebar, state.projectsSidebar, paneOptions)).some((pane) => pane.kind === "context")
}

/** The rule of a band input without a status band: `─` across, `composer.border_info` at the right end. */
export function bandRule(state: AppState, columns: number, env: Record<string, string | undefined> = process.env): string {
  const info = borderInfoText(state, state.structure.composer.border_info, Math.max(0, columns - 4), env)
  return "─".repeat(Math.max(0, columns - Bun.stringWidth(info) - (info ? 1 : 0))) + info + (info ? "─" : "")
}
