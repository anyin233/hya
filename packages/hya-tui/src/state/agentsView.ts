/**
 * The Agents view (`/agent`, docs/tui.md "Agents view"): pure state and keys
 * over `GET /v1/agent-models` (components/AgentsView.tsx renders it;
 * app/agentsView.ts owns the calls and the pickers `m` and `t` open).
 *
 * One screen, three sections under titled divider rules: primary agents,
 * subagents, and system agents (the hidden `compaction`, `summary`, `title`).
 * Enter selects a primary agent for the session; subagents and system agents
 * only run when the harness starts them, so Enter on them notices why. `m`
 * picks the agent's default model (a remembered preference, or the owning
 * config file for a `configured` agent), `t` its default thinking effort, and
 * `c` clears a remembered preference.
 */
import type { AgentModelState } from "../client"
import type { KeyLike } from "../keys/bindings"
import { truncate } from "./format"

export interface AgentsViewBusy {
  label: string
  startedAt: number
}

export interface AgentsViewNotice {
  text: string
  tone: "info" | "ok" | "error"
}

export interface AgentsViewState {
  agent: string | undefined
  filter: string
  filtering: boolean
  busy?: AgentsViewBusy
  notice?: AgentsViewNotice
}

export type AgentsViewOutcome =
  | { type: "none" }
  | { type: "update"; view: AgentsViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "refresh" }
  | { type: "select"; agent: string }
  | { type: "pickModel"; agent: string }
  | { type: "clear"; agent: string }
  | { type: "pickEffort"; agent: string }

export interface AgentsViewKeyRow {
  keys: string
  description: string
  hint?: string
}

export const agentsViewKeyRows: readonly AgentsViewKeyRow[] = [
  { keys: "Up / Down", description: "Move the highlight over agents (divider rules are skipped)", hint: "↑↓ move" },
  { keys: "Enter", description: "Select this primary agent for the session", hint: "Enter select" },
  { keys: "m", description: "Pick the agent's default model (saved to config.yaml for a pinned agent)", hint: "m model" },
  { keys: "t", description: "Pick the agent's default thinking effort (default clears it)", hint: "t effort" },
  { keys: "c", description: "Clear the agent's remembered model", hint: "c clear" },
  { keys: "r", description: "Refresh the list", hint: "r refresh" },
  { keys: "/", description: "Filter the rows by typing; Enter keeps the filter, Esc clears it", hint: "/ filter" },
  { keys: "Esc", description: "Cancel a running call, clear the filter, close the view" },
]

export type AgentSection = "primary" | "subagent" | "system"

const sectionTitles: Record<AgentSection, string> = {
  primary: "Primary agents",
  subagent: "Subagents",
  system: "System agents",
}

const sectionOrder: readonly AgentSection[] = ["primary", "subagent", "system"]

/** Hidden agents are system agents; the rest split by their selector mode. */
export function agentSection(row: AgentModelState): AgentSection {
  if (row.hidden) return "system"
  return row.mode === "subagent" ? "subagent" : "primary"
}

/** One line of the view: a section's divider rule or an agent row. */
export type AgentsViewLine =
  | { kind: "divider"; section: AgentSection; title: string }
  | { kind: "agent"; row: AgentModelState }

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

function cell(text: string, width: number): string {
  const cut = truncate(text, width)
  return cut + " ".repeat(Math.max(0, width - Bun.stringWidth(cut)))
}

/** `AgentEffortSource` in words; empty when the model's default applies. */
export function effortSourceText(source: string | undefined): string {
  switch ((source ?? "").replace(/^AGENT_EFFORT_SOURCE_/, "")) {
    case "PREFERENCE": return "set"
    case "CONFIGURED": return "config"
    case "AUTHORED": return "bundle"
    default: return ""
  }
}

/** The agent's default effort with its layer, e.g. `high (set)`; `default` when the model decides. */
export function effortText(row: AgentModelState): string {
  if (!row.effort) return "default"
  const source = effortSourceText(row.effortSource)
  return source ? `${row.effort} (${source})` : row.effort
}

/**
 * The effective-model column's width: what `width` leaves after the agent id
 * (28, the longest built-in id), source (11), and effort (13, `high (config)`)
 * columns and their separators, between 12 and 28, so the effort column
 * stays whole at about 80 terminal columns.
 */
function modelColumn(width: number): number {
  return Math.max(12, Math.min(28, width - 28 - 11 - 13 - 3))
}

/** One agent row: id, effective model, source, effort (fits `width`). */
export function agentLine(row: AgentModelState, width: number): string {
  const line = `${cell(row.agentId, 28)} ${cell(effectiveRef(row), modelColumn(width))} ${cell(sourceText(row.source), 11)} ${effortText(row)}`
  return truncate(line.trimEnd(), width)
}

export function agentHeaderLine(width: number): string {
  return truncate(`${cell("AGENT", 28)} ${cell("EFFECTIVE MODEL", modelColumn(width))} ${cell("SOURCE", 11)} EFFORT`, width)
}

/** A section's divider rule, `── Title ───…`, filling `width`. */
export function sectionRule(title: string, width: number): string {
  const head = `── ${title} `
  return truncate(head + "─".repeat(Math.max(0, width - Bun.stringWidth(head))), width)
}

function matches(haystack: string, filter: string): boolean {
  const words = filter.toLowerCase().split(/\s+/).filter(Boolean)
  const text = haystack.toLowerCase()
  return words.every((word) => text.includes(word))
}

/** Agents shown now in display order (sections in order, server order within one), filtered by id, section, or effective model. */
export function shownAgents(view: Pick<AgentsViewState, "filter">, rows: readonly AgentModelState[]): AgentModelState[] {
  const kept = view.filter
    ? rows.filter((row) => matches(`${row.agentId}\n${sectionTitles[agentSection(row)]}\n${effectiveRef(row)}`, view.filter))
    : rows
  return sectionOrder.flatMap((section) => kept.filter((row) => agentSection(row) === section))
}

/** The view's lines: each non-empty section's divider followed by its agents. */
export function agentsViewLines(view: Pick<AgentsViewState, "filter">, rows: readonly AgentModelState[]): AgentsViewLine[] {
  const shown = shownAgents(view, rows)
  return sectionOrder.flatMap((section): AgentsViewLine[] => {
    const members = shown.filter((row) => agentSection(row) === section)
    if (!members.length) return []
    return [{ kind: "divider", section, title: sectionTitles[section] }, ...members.map((row): AgentsViewLine => ({ kind: "agent", row }))]
  })
}

/** Open on the session's agent when it is listed, else the first agent. */
export function initialAgentsView(rows: readonly AgentModelState[], current?: string): AgentsViewState {
  const shown = shownAgents({ filter: "" }, rows)
  const agent = shown.find((row) => row.agentId === current)?.agentId ?? shown[0]?.agentId
  return { agent, filter: "", filtering: false }
}

/** Keep the highlight on its row after a reload or filter change. */
export function settleAgentsView(view: AgentsViewState, rows: readonly AgentModelState[]): AgentsViewState {
  const shown = shownAgents(view, rows)
  return shown.some((row) => row.agentId === view.agent) || !shown.length ? view : { ...view, agent: shown[0]!.agentId }
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"
const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

function move(view: AgentsViewState, rows: readonly AgentModelState[], step: number): AgentsViewState {
  const shown = shownAgents(view, rows)
  if (!shown.length) return view
  const at = shown.findIndex((row) => row.agentId === view.agent)
  return { ...view, agent: shown[(at + step + shown.length) % shown.length]!.agentId }
}

const notice = (view: AgentsViewState, text: string): AgentsViewOutcome => ({ type: "update", view: { ...view, notice: { tone: "info", text } } })

/** One key while the Agents view is open. */
export function agentsViewKey(view: AgentsViewState, key: KeyLike, rows: readonly AgentModelState[]): AgentsViewOutcome {
  if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }
  if (key.name === "up") return { type: "update", view: move(view, rows, -1) }
  if (key.name === "down") return { type: "update", view: move(view, rows, 1) }
  if (view.filtering) {
    if (key.name === "escape") return { type: "update", view: settleAgentsView({ ...view, filtering: false, filter: "" }, rows) }
    if (isEnter(key)) return { type: "update", view: { ...view, filtering: false } }
    if (key.name === "backspace") return { type: "update", view: settleAgentsView({ ...view, filter: view.filter.slice(0, -1) }, rows) }
    if (printable(key)) return { type: "update", view: settleAgentsView({ ...view, filter: view.filter + key.sequence }, rows) }
    return { type: "none" }
  }
  if (key.name === "escape") {
    if (view.filter) return { type: "update", view: settleAgentsView({ ...view, filter: "" }, rows) }
    return { type: "close" }
  }
  if (key.ctrl || key.meta) return { type: "none" }
  if (key.sequence === "/") return { type: "update", view: { ...view, filtering: true } }
  if (key.sequence === "r") return { type: "refresh" }
  const row = shownAgents(view, rows).find((candidate) => candidate.agentId === view.agent)
  if (!row) return notice(view, "No agent selected")
  if (isEnter(key)) {
    const section = agentSection(row)
    if (section === "primary") return { type: "select", agent: row.agentId }
    return notice(view, `${row.agentId} is a ${section === "system" ? "system agent" : "subagent"}; only primary agents run a session`)
  }
  if (key.sequence === "m") return { type: "pickModel", agent: row.agentId }
  if (key.sequence === "t") return { type: "pickEffort", agent: row.agentId }
  if (key.sequence === "c") {
    if (row.configured) return notice(view, `${row.agentId}'s model is pinned in ${row.configurationPath || "its configuration"}; m changes it`)
    if (!row.preference?.providerId) return notice(view, `${row.agentId} has no remembered preference`)
    return { type: "clear", agent: row.agentId }
  }
  return { type: "none" }
}

/** The footer hint for the current screen, filter, or busy call. */
export function agentsViewHint(view: AgentsViewState): string {
  if (view.busy) return `${view.busy.label}… · Esc cancels`
  if (view.filtering) return "Type to filter · Enter keeps the filter · Esc clears it"
  return [...agentsViewKeyRows.filter((row) => row.hint).map((row) => row.hint!), "Esc close"].join(" · ")
}
