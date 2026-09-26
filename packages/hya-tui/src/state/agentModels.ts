/**
 * The Agent Models view (`/agent-models`, docs/tui.md "Agent Models"): pure
 * state and keys over `GET /v1/agent-models` / `PUT /v1/agent-models/{id}`
 * (components/AgentModelsView.tsx renders it; app/agentModels.ts owns the
 * calls, including the model picker Enter opens).
 *
 * One screen: every catalog agent, its effective model and which tier
 * resolved it. Enter on a `settable` agent opens the shared model picker to
 * choose its remembered default; `c` clears a set preference. An agent with
 * direct configuration (`configured`) cannot take one; both keys notice why
 * instead of acting.
 */
import type { AgentModelState } from "../client"
import type { KeyLike } from "../keys/bindings"
import { truncate } from "./format"

export interface AgentModelsBusy {
  label: string
  startedAt: number
}

export interface AgentModelsNotice {
  text: string
  tone: "info" | "ok" | "error"
}

export interface AgentModelsViewState {
  agent: string | undefined
  filter: string
  filtering: boolean
  busy?: AgentModelsBusy
  notice?: AgentModelsNotice
}

export type AgentModelsViewOutcome =
  | { type: "none" }
  | { type: "update"; view: AgentModelsViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "refresh" }
  | { type: "pickModel"; agent: string }
  | { type: "clear"; agent: string }

export interface AgentModelsKeyRow {
  keys: string
  description: string
  hint?: string
}

export const agentModelsKeyRows: readonly AgentModelsKeyRow[] = [
  { keys: "Up / Down", description: "Move the highlight over agents", hint: "↑↓ move" },
  { keys: "Enter", description: "Pick this agent's remembered default model", hint: "Enter pick" },
  { keys: "c", description: "Clear the agent's remembered preference", hint: "c clear" },
  { keys: "r", description: "Refresh the list", hint: "r refresh" },
  { keys: "/", description: "Filter the rows by typing; Enter keeps the filter, Esc clears it", hint: "/ filter" },
  { keys: "Esc", description: "Cancel a running call, clear the filter, close the view" },
]

/** `AgentModelSource` in words. */
export function sourceText(source: string | undefined): string {
  const name = (source ?? "").replace(/^AGENT_MODEL_SOURCE_/, "")
  switch (name) {
    case "SESSION": return "session"
    case "CONFIGURED": return "configured"
    case "REMEMBERED": return "remembered"
    case "DEFAULT": return "default"
    default: return "—"
  }
}

/** The effective `provider/model` reference; `—` when unset. */
export function effectiveRef(row: AgentModelState): string {
  const model = row.effective
  return model?.providerId && model.modelId ? `${model.providerId}/${model.modelId}` : "—"
}

/** Why `row` cannot take a remembered preference; `undefined` when it can. */
export function notSettableReason(row: AgentModelState): string | undefined {
  if (row.settable) return undefined
  if (row.configured) return "has direct model/category configuration"
  return "cannot take a remembered preference"
}

function cell(text: string, width: number): string {
  const cut = truncate(text, width)
  return cut + " ".repeat(Math.max(0, width - Bun.stringWidth(cut)))
}

/** One agent row: id, mode, effective model, source (fits `width`). */
export function agentModelLine(row: AgentModelState, width: number): string {
  const reason = notSettableReason(row)
  const line = `${cell(row.agentId, 16)} ${cell(row.mode || "—", 10)} ${cell(effectiveRef(row), 28)} ${cell(sourceText(row.source), 12)} ${reason ? `(${reason})` : ""}`
  return truncate(line.trimEnd(), width)
}

export function agentModelHeaderLine(width: number): string {
  return truncate(`${cell("AGENT", 16)} ${cell("MODE", 10)} ${cell("EFFECTIVE MODEL", 28)} ${cell("SOURCE", 12)} NOTE`, width)
}

function matches(haystack: string, filter: string): boolean {
  const words = filter.toLowerCase().split(/\s+/).filter(Boolean)
  const text = haystack.toLowerCase()
  return words.every((word) => text.includes(word))
}

/** Agents shown now, filtered by id, mode, or effective model. */
export function shownAgentModels(view: Pick<AgentModelsViewState, "filter">, rows: readonly AgentModelState[]): AgentModelState[] {
  if (!view.filter) return [...rows]
  return rows.filter((row) => matches(`${row.agentId}\n${row.mode ?? ""}\n${effectiveRef(row)}`, view.filter))
}

export function initialAgentModelsView(rows: readonly AgentModelState[]): AgentModelsViewState {
  return { agent: shownAgentModels({ filter: "" }, rows)[0]?.agentId, filter: "", filtering: false }
}

/** Keep the highlight on its row after a reload. */
export function settleAgentModelsView(view: AgentModelsViewState, rows: readonly AgentModelState[]): AgentModelsViewState {
  const shown = shownAgentModels(view, rows)
  return shown.some((row) => row.agentId === view.agent) || !shown.length ? view : { ...view, agent: shown[0]!.agentId }
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"
const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

function move(view: AgentModelsViewState, rows: readonly AgentModelState[], step: number): AgentModelsViewState {
  const shown = shownAgentModels(view, rows)
  if (!shown.length) return view
  const at = shown.findIndex((row) => row.agentId === view.agent)
  return { ...view, agent: shown[(at + step + shown.length) % shown.length]!.agentId }
}

/** One key while the Agent Models view is open. */
export function agentModelsViewKey(view: AgentModelsViewState, key: KeyLike, rows: readonly AgentModelState[]): AgentModelsViewOutcome {
  if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }
  if (key.name === "up") return { type: "update", view: move(view, rows, -1) }
  if (key.name === "down") return { type: "update", view: move(view, rows, 1) }
  if (view.filtering) {
    if (key.name === "escape") return { type: "update", view: settleAgentModelsView({ ...view, filtering: false, filter: "" }, rows) }
    if (isEnter(key)) return { type: "update", view: { ...view, filtering: false } }
    if (key.name === "backspace") return { type: "update", view: settleAgentModelsView({ ...view, filter: view.filter.slice(0, -1) }, rows) }
    if (printable(key)) return { type: "update", view: settleAgentModelsView({ ...view, filter: view.filter + key.sequence }, rows) }
    return { type: "none" }
  }
  if (key.name === "escape") {
    if (view.filter) return { type: "update", view: settleAgentModelsView({ ...view, filter: "" }, rows) }
    return { type: "close" }
  }
  if (key.ctrl || key.meta) return { type: "none" }
  if (key.sequence === "/") return { type: "update", view: { ...view, filtering: true } }
  if (key.sequence === "r") return { type: "refresh" }
  const row = rows.find((candidate) => candidate.agentId === view.agent)
  if (!row) return { type: "update", view: { ...view, notice: { tone: "info", text: "No agent selected" } } }
  if (isEnter(key)) {
    const reason = notSettableReason(row)
    if (reason) return { type: "update", view: { ...view, notice: { tone: "info", text: `${row.agentId} ${reason}` } } }
    return { type: "pickModel", agent: row.agentId }
  }
  if (key.sequence === "c") {
    const reason = notSettableReason(row)
    if (reason) return { type: "update", view: { ...view, notice: { tone: "info", text: `${row.agentId} ${reason}` } } }
    if (!row.preference?.providerId) return { type: "update", view: { ...view, notice: { tone: "info", text: `${row.agentId} has no remembered preference` } } }
    return { type: "clear", agent: row.agentId }
  }
  return { type: "none" }
}

/** The footer hint for the current screen, filter, or busy call. */
export function agentModelsViewHint(view: AgentModelsViewState): string {
  if (view.busy) return `${view.busy.label}… · Esc cancels`
  if (view.filtering) return "Type to filter · Enter keeps the filter · Esc clears it"
  return [...agentModelsKeyRows.filter((row) => row.hint).map((row) => row.hint!), "Esc close"].join(" · ")
}
