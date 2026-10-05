/** Pure text for the sidebar, pending block, and non-chat views, derived from the store. */
import type { WebInfo } from "../cli"
import type { Interaction, ModelSummary, SessionInfo, TodoItem, TokenUsage } from "../client"
import { keyHelpText } from "../commands/help"
import type { View } from "../instructions"
import { mergeTranscript } from "./overlay"
import { promptQueue } from "./prompts"
import { sessionsInScope } from "./projects"
import { forkSourceText } from "./revert"
import type { AppState } from "./store"

export function modelBaseReference(session: SessionInfo): string {
  const model = session.model
  return model?.providerId && model.modelId ? `${model.providerId}/${model.modelId}` : ""
}

export function modelReference(session: SessionInfo): string {
  const base = modelBaseReference(session)
  return base ? `${base}${session.model?.variant ? `#${session.model.variant}` : ""}` : ""
}

/**
 * The session's thinking effort with the layer that chose it, e.g. `low
 * (pref)`, for the `/status` Thinking row. The effort is the server-resolved
 * `effectiveEffort`; `default` means no request effort (the provider's own
 * default applies), while `none` is an explicit off switch.
 */
export function thinkingEffortLabel(session: SessionInfo | undefined): string {
  if (!session?.effectiveEffort) return "default"
  const source = session.effortSource?.replace(/^EFFORT_SOURCE_/, "").toLowerCase()
  return source && source !== "unspecified" && source !== "none" ? `${session.effectiveEffort} (${source === "preference" ? "pref" : source.replace("_", " ")})` : session.effectiveEffort
}

/**
 * The session's model followed by its thinking effort, `provider/model:effort`
 * (`short`: `model:effort`), e.g. `openai/gpt-6-astra:max`; "" with no model.
 * The header and the status bar show it, so the effort in use is always
 * visible right after the model name.
 */
export function modelEffortLabel(session: SessionInfo, short = false): string {
  const model = session.model
  if (!model?.providerId || !model.modelId) return ""
  return `${short ? model.modelId : `${model.providerId}/${model.modelId}`}:${session.effectiveEffort || "default"}`
}

/** The open session's model catalog row, matched without its optional effort suffix. */
export function currentModel(state: Pick<AppState, "selected" | "models">): ModelSummary | undefined {
  const session = state.selected
  if (!session) return undefined
  const ref = modelBaseReference(session)
  return ref ? state.models.find((model) => model.id === ref) : undefined
}

/** Cut `text` to `width` columns with a trailing `…` (no-op when it fits or width is unset). */
export function truncate(text: string, width?: number): string {
  if (width === undefined || text.length <= width) return text
  return width <= 1 ? "…".slice(0, width) : `${text.slice(0, width - 1)}…`
}

/** Cut from the left, keeping the end (paths): `…/work`. */
export function truncateStart(text: string, width: number): string {
  return text.length <= width ? text : `…${text.slice(text.length - width + 1)}`
}

/** What the status line and the Context box call the server: its label (`--server-label`), else its URL, else `fallback`. */
export function shownServer(state: AppState, fallback: string): string {
  return state.serverLabel || state.serverUrl || fallback
}

export interface SessionRow {
  session: SessionInfo
  /** 0 for a top-level session, 1 for its subagents, 2 for theirs. */
  depth: number
  /** Hierarchical display number: roots are 1-based, descendants use x.y notation. */
  number: string
}

/**
 * Sessions as a tree in stable creation order. A running session's updates must
 * not renumber every other session in the sidebar or `/open`.
 */
export function sessionTree(sessions: readonly SessionInfo[]): SessionRow[] {
  const ids = new Set(sessions.map((session) => session.id))
  const children = new Map<string, SessionInfo[]>()
  for (const session of sessions) {
    if (session.parent && ids.has(session.parent) && session.parent !== session.id) {
      children.set(session.parent, [...(children.get(session.parent) ?? []), session])
    }
  }
  const created = (session: SessionInfo): number => Date.parse(session.timeCreated ?? "") || 0
  const stable = (a: SessionInfo, b: SessionInfo): number => created(a) - created(b) || a.id.localeCompare(b.id)
  for (const group of children.values()) group.sort(stable)
  const roots = sessions.filter((session) => !(session.parent && ids.has(session.parent))).sort(stable)
  const rows: SessionRow[] = []
  const seen = new Set<string>()
  const visit = (session: SessionInfo, depth: number, number: string): void => {
    if (seen.has(session.id)) return
    seen.add(session.id)
    rows.push({ session, depth, number })
    let childNumber = 0
    for (const child of children.get(session.id) ?? []) {
      childNumber += 1
      visit(child, depth + 1, `${number}.${childNumber}`)
    }
  }
  const nextRootNumber = (): string => String(rows.filter((row) => row.depth === 0).length + 1)
  for (const session of roots) visit(session, 0, nextRootNumber())
  for (const session of sessions) if (!seen.has(session.id)) visit(session, 0, nextRootNumber())
  return rows
}


/** `/to-background` and Ctrl+D in a WebUI tab. */
export const webTabBackgroundNotice = "Close the tab to leave this session running"
/**
 * One line per pending interaction the prompt does not show (asks of other
 * session trees): `! title · session · id` for permissions, `? title ·
 * session` for questions, where a listed session uses its `/open` number and
 * title; an archived/unlisted session says `saved session` instead of a raw id.
 */
export function pendingLines(state: AppState, width?: number): string[] {
  const prompted = new Set(promptQueue(state.interactions, state).map((item) => item.id))
  return state.interactions.filter((item) => !prompted.has(item.id)).map((item) => {
    const label = item.session ? askSessionLabel(item.session, state.sessions, state.activeProjectId) : undefined
    const session = label ? ` · ${label === item.session ? "saved session" : label}` : ""
    return truncate(`${item.type?.includes("QUESTION") ? "?" : "!"} ${item.title}${session}`, width)
  })
}

/**
 * The session numbers the sidebar shows and `/open <n>` takes: the tree of
 * the active Project's sessions (state/projects.ts `sessionsInScope`, with
 * every temporary session), roots `1`, `2`, … and subagents `2.1`, `2.1.3`.
 * A session of another Project has none.
 */
export function sessionNumbers(sessions: readonly SessionInfo[], activeProjectId: string | undefined): Map<string, string> {
  return new Map(sessionTree(sessionsInScope(sessions, activeProjectId, false)).map((row) => [row.session.id, row.number]))
}

/** Which session an ask belongs to: `<n>. <title>`, its title (or id) when it has no number here, or its id when unlisted. */
export function askSessionLabel(sessionId: string, sessions: readonly SessionInfo[], activeProjectId: string | undefined): string {
  const session = sessions.find((row) => row.id === sessionId)
  if (!session) return sessionId
  const number = sessionNumbers(sessions, activeProjectId).get(sessionId)
  return number === undefined ? session.title || session.id : `${number}. ${session.title || session.id}`
}

/** Status line when an ask arrives for another session: /pending opens its normal prompt. */
export function otherAskNotice(interaction: Interaction, sessions: readonly SessionInfo[], activeProjectId: string | undefined): string {
  const sessionId = interaction.session ?? ""
  const kind = interaction.type?.includes("QUESTION") ? "Question" : "Permission needed"
  const label = askSessionLabel(sessionId, sessions, activeProjectId)
  return `${kind} in ${label === sessionId ? "a saved session" : label} · /pending to review`
}

/** `WebUI http://127.0.0.1:3250` (status line), or `WebUI unavailable`; `undefined` without a WebUI. */
export function webLabel(web: WebInfo | undefined): string | undefined {
  if (!web) return undefined
  return web.url ? `WebUI ${web.url.replace(/\/$/, "")}` : "WebUI unavailable"
}

/** The warning shown when bare `hya` could not start the WebUI; `undefined` otherwise. */
export function webNotice(web: WebInfo | undefined): string | undefined {
  if (!web || web.url || web.error === undefined) return undefined
  return `WebUI unavailable: ${web.error} · hya --port <N>`
}

const titles: Record<View, string> = {
  chat: "Chat", models: "Models", workflows: "Workflows", interactions: "Interactions",
  api: "API commands", help: "Help", todos: "Todos", status: "Status",
}

/** `TODO_STATUS_IN_PROGRESS` → `in_progress`, etc. */
export function todoStatusText(status: string): string {
  return status.replace(/^[A-Z_]*STATUS_/, "").toLowerCase() || "pending"
}

/**
 * Status glyphs for the todo panel (sidebar "Todos" box and the `/todos`
 * view): pending `○`, in progress `◐`, completed `✓`, blocked `✗` (the
 * `TodoStatus` enum has no `cancelled` status; `blocked` takes its glyph).
 */
export const todoGlyphs: Record<string, string> = {
  pending: "○", in_progress: "◐", blocked: "✗", completed: "✓",
}

/**
 * `CompactionApplied.strategy` (`docs/protocol/README.md` "Compaction") in
 * words: `native` (provider-native compaction), `local_summarizer` (a
 * model-written summary — also every manual `/compact`), `snap_compact` (a
 * local dense archive, no model call), or `handoff` (a model-written handoff
 * document); an unrecognized or missing name is shown as-is (or `unknown`).
 */
export function strategyText(strategy: string | undefined): string {
  switch (strategy) {
    case "native": return "native"
    case "local_summarizer": return "local summary"
    case "snap_compact": return "snapshot"
    case "handoff": return "handoff"
    default: return strategy || "unknown"
  }
}

/**
 * A `CompactionApplied` transcript divider:
 * `── context compacted · 12 messages · manual · local summary ──` for a
 * `/compact` (or `/summarize`), without `manual` for a mid-turn compaction;
 * the count is omitted when the event does not carry one. The strategy name
 * is always shown (`strategyText`) — manual and automatic compactions both
 * carry one.
 */
export function compactionText(payload: { untilSeq?: string; strategy?: string; foldedCount?: number | string; manual?: boolean }): string {
  const count = Number(payload.foldedCount ?? 0)
  const folded = count > 0 ? ` · ${count} message${count === 1 ? "" : "s"}` : ""
  const manual = payload.manual ? " · manual" : ""
  return `── context compacted${folded}${manual} · ${strategyText(payload.strategy)} ──`
}

/**
 * The divider of a compaction from before the session was opened, derived
 * from its summary message (state/messages.ts `withDividers`): the strategy
 * and folded count live only in the `CompactionApplied` event, which the
 * TUI does not replay.
 */
export const historyCompactionText = "── context compacted ──"

/** A uint64 count (a decimal string) as a number; exact up to 2^53, which token counts never reach. */
const count = (value: string | number | undefined): number => Number(value ?? 0) || 0

/** `950`, `12.3k`, `123k`, `1.2M` for a token count (a uint64 decimal string or a number). */
export function formatTokens(value: string | number): string {
  const n = count(value)
  if (n < 1000) return String(n)
  if (n < 100_000) return `${(Math.floor(n / 100) / 10).toFixed(1).replace(/\.0$/, "")}k`
  if (n < 1_000_000) return `${Math.floor(n / 1000)}k`
  return `${(Math.floor(n / 100_000) / 10).toFixed(1).replace(/\.0$/, "")}M`
}

/** Prompt tokens of one round: `input + cacheRead + cacheWrite` (`input` excludes the cache). */
export function promptTokens(usage: TokenUsage | undefined): number {
  return count(usage?.input) + count(usage?.cacheRead) + count(usage?.cacheWrite)
}

/** Everything billed for a session (`SessionInfo.usage`): prompt tokens plus output; `undefined` when unknown or zero. */
export function sessionTokens(usage: TokenUsage | undefined): number | undefined {
  const total = promptTokens(usage) + count(usage?.output)
  return total > 0 ? total : undefined
}

export interface ContextUsage {
  /** Rounded percentage of the context window the latest round's prompt used. */
  percent: number
  tokens: number
  limit: number
}

/**
 * Context occupancy (E22; docs/protocol/README.md "Usage and context
 * occupancy"): the prompt of the latest provider round against the context
 * limit of the model that served it. Live, the newest `tokensRecorded` with
 * a message (`state.liveRound`); otherwise the newest assistant message with
 * `roundUsage`. `undefined` when the model's limit or the usage is unknown.
 */
export function contextUsage(state: Pick<AppState, "liveRound" | "messages" | "overlay" | "models">): ContextUsage | undefined {
  let round = state.liveRound
  if (!round) {
    const message = [...mergeTranscript(state.messages, state.overlay)].reverse()
      .find((candidate) => candidate.role === "ROLE_ASSISTANT" && candidate.roundUsage)
    if (message) round = { message: message.id, model: message.model ?? "", usage: message.roundUsage! }
  }
  if (!round) return undefined
  const limit = count(state.models.find((model) => model.id === round.model)?.contextLimit)
  const tokens = promptTokens(round.usage)
  if (!limit || !tokens) return undefined
  return { percent: Math.round((tokens * 100) / limit), tokens, limit }
}

/** One line per todo item: a status glyph and its content. */
export function todosText(items: { content: string; status: string }[]): string {
  if (!items.length) return "No todos for this session."
  return items.map((item) => {
    const status = todoStatusText(item.status)
    return `${todoGlyphs[status] ?? "·"} ${item.content}`
  }).join("\n")
}

export function mainTitle(view: View): string { return titles[view] }

export function mainContent(state: AppState, view: View = state.view): string {
  if (!state.ready) return ""
  switch (view) {
    case "chat":
      // The transcript itself is rendered per message (components/Transcript.tsx).
      return state.messages.length || state.overlay.length || state.queued.length ? "" : "No messages yet. Type a prompt below."
    case "models":
      return state.models.length ? state.models.map((model) => `${model.id}  ${model.displayName ?? ""}  ${model.auth ?? ""}`).join("\n") : "No models returned by server."
    case "workflows": {
      const current = state.workflowState
      return `${current ? `Current: ${String(current.workflow ?? "none")} · ${String(current.status ?? "") }\n\n` : ""}${state.workflows.length ? state.workflows.map((workflow) => `${workflow.name} · ${workflow.stageCount ?? 0} stages\n${workflow.description ?? ""}`).join("\n\n") : "No workflows discovered."}`
    }
    case "interactions":
      return state.interactions.length ? state.interactions.map((item) => `${item.type} · ${item.id}\n${item.title}\n${item.detail ?? ""}\n${(item.options ?? []).join(" | ")}`).join("\n\n") : "No pending interactions."
    case "api": return state.apiOutput
    case "help": return keyHelpText()
    case "todos": return todosText(state.todos)
    case "status": return state.statusText
  }
}

/** The concealed entry's line: at most 32 bullets (fits 80 columns) and the character count. */
export function secretMask(length: number): string {
  if (!length) return " "
  return `${"•".repeat(Math.min(length, 32))}${length > 32 ? "…" : ""}  ${length} character${length === 1 ? "" : "s"}`
}
