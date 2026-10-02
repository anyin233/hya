/**
 * The session context shown in exactly one place at a time (docs/tui.md
 * "Layout"): the right sidebar's `Context` pane when it is on screen, else the
 * top status line of the Conversation pane. Both render the same
 * `contextFields()`: the box as one labelled row per field, the status line as
 * compact ` · `-separated segments packed onto at most two rows.
 */
import { contextUsage, formatTokens, modelEffortLabel, sessionTokens, todoStatusText, truncate, truncateStart, webLabel } from "./format"
import { mergeTranscript } from "./overlay"
import { effectiveMode, modeDisplay } from "./modes"
import { forkSourceText } from "./revert"
import type { AppState } from "./store"
import { tuiVersion } from "../version"

/**
 * How a field is colored: `plain` is the box's text color and the status
 * line's muted color; `strong` is the text color on both; the rest are the
 * theme colors of the same name.
 */
export type ContextTone = "plain" | "strong" | "accent" | "warning" | "error"

export interface ContextField {
  /** Box row label (`Session`, `Mode`, …). */
  label: string
  /** Box row value. */
  value: string
  /** Status line segment (`mode manual`, `ctx 42%`, `⎇ main`, …). */
  short: string
  tone: ContextTone
  /** Which end of `value` the box cuts when it is too long: paths and ids lose their start (`…/work`), names their end. */
  cut: "start" | "end"
  /** Drop order on a narrow status line: the highest number drops first; 0 is never dropped. */
  priority: number
}

export interface ContextRow {
  /** The label padded to the value column. */
  label: string
  value: string
  tone: ContextTone
}

/** The Context box's label column (`Messages` plus one space). */
const labelWidth = 9
/** The status line's directory segment keeps this many columns of the path's tail. */
const directoryWidth = 24
/** Context percent from which occupancy warns (warning color) and alarms (error color). */
export const contextWarnPercent = 80
export const contextAlarmPercent = 95

const hostOf = (url: string): string => url.replace(/^https?:\/\//, "").replace(/\/$/, "")

/**
 * The ordered fields: Vim (when on), Mode, Session, Forked, Agent, Model,
 * Messages, Context, Tokens, Dir, Branch, Todos, Server, WebUI, Version, Backend.
 * Fields with no data are omitted.
 */
export function contextFields(state: AppState, server: string): ContextField[] {
  const fields: ContextField[] = []
  const add = (field: Omit<ContextField, "tone" | "cut"> & Partial<Pick<ContextField, "tone" | "cut">>): void => {
    fields.push({ tone: "plain", cut: "start", ...field })
  }
  if (state.vim) {
    const normal = state.vimMode === "normal"
    const mode = `${normal ? "NORMAL" : "INSERT"}${state.vimPending ? ` ${state.vimPending}` : ""}`
    add({ label: "Vim", value: mode, short: `-- ${normal ? "NORMAL" : "INSERT"} --${state.vimPending ? ` ${state.vimPending}` : ""}`, tone: normal ? "accent" : "plain", cut: "end", priority: 0 })
  }
  const mode = modeDisplay(effectiveMode(state), state.permissionModes)
  add({ label: "Mode", value: mode.text, short: `mode ${mode.text}`, tone: mode.tone === "error" ? "error" : mode.tone === "accent" ? "accent" : "strong", cut: "end", priority: 0 })
  const session = state.selected
  const title = session ? session.title || session.id : state.ready ? "none" : "connecting…"
  add({ label: "Session", value: title, short: title, tone: "accent", priority: 1 })
  if (session) {
    const forked = forkSourceText(session.forkedFrom, state.sessions)
    if (forked) add({ label: "Forked", value: forked.replace(/^forked /, ""), short: forked, cut: "end", priority: 9 })
    add({ label: "Agent", value: session.agent, short: session.agent, priority: 4 })
    const model = modelEffortLabel(session)
    add({ label: "Model", value: model || "default", short: modelEffortLabel(session, true) || "default", cut: "end", priority: 2 })
    const messages = mergeTranscript(state.messages, state.overlay).length
    add({ label: "Messages", value: String(messages), short: `${messages} msg${messages === 1 ? "" : "s"}`, priority: 8 })
    const usage = contextUsage(state)
    if (usage) {
      const tone = usage.percent >= contextAlarmPercent ? "error" : usage.percent >= contextWarnPercent ? "warning" : "plain"
      add({ label: "Context", value: `${usage.percent}% · ${formatTokens(usage.tokens)}/${formatTokens(usage.limit)}`, short: `ctx ${usage.percent}%`, tone, priority: 3 })
    }
    const tokens = sessionTokens(session.usage)
    if (tokens !== undefined) add({ label: "Tokens", value: formatTokens(tokens), short: `${formatTokens(tokens)} tok`, priority: 5 })
    if (session.workdir) add({ label: "Dir", value: session.workdir, short: truncateStart(session.workdir, directoryWidth), priority: 6 })
  }
  if (state.gitBranch) add({ label: "Branch", value: state.gitBranch, short: `⎇ ${state.gitBranch}`, priority: 7 })
  if (state.todos.length) {
    const done = state.todos.filter((item) => todoStatusText(item.status) === "completed").length
    add({ label: "Todos", value: `${done}/${state.todos.length}`, short: `Todos ${done}/${state.todos.length}`, priority: 7 })
  }
  add({ label: "Server", value: hostOf(server), short: hostOf(server), priority: 9 })
  if (state.web) {
    add({ label: "WebUI", value: state.web.url ? hostOf(state.web.url) : "unavailable", short: webLabel(state.web)!, tone: state.web.url ? "plain" : "warning", priority: 4 })
  }
  const version = `${tuiVersion}/${state.serverVersion || "unknown"}`
  add({ label: "Version", value: version, short: version, priority: 1 })
  if (state.backendStopped) add({ label: "Backend", value: "stopped", short: "backend stopped", tone: "error", priority: 2 })
  else if (!state.connected) add({ label: "Backend", value: "reconnecting", short: "reconnecting", tone: "warning", priority: 2 })
  return fields
}

/** The Context box: one row per field, the value cut to `width` minus the label column. */
export function contextRows(fields: readonly ContextField[], width: number): ContextRow[] {
  const room = Math.max(4, width - labelWidth)
  return fields.map((field) => ({
    label: field.label.padEnd(labelWidth),
    value: field.cut === "start" ? truncateStart(field.value, room) : truncate(field.value, room),
    tone: field.tone,
  }))
}


/** Compact Context metadata for the top status line when the sidebar is hidden. */
export function contextStatus(fields: readonly ContextField[], width: number): string {
  const visible = fields.slice().sort((left, right) => left.priority - right.priority)
  const kept: string[] = []
  let room = Math.max(1, width)
  for (const field of visible) {
    const separator = kept.length ? 3 : 0
    if (field.priority > 0 && field.short.length + separator > room) continue
    kept.push(field.short)
    room -= field.short.length + separator
  }
  return kept.join(" · ")
}
