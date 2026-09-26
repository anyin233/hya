/** Pure text for the header, sidebar, pending block, and non-chat views, derived from the store. */
import type { WebInfo } from "../cli"
import type { Interaction, ModelSummary, SessionInfo, TodoItem, TokenUsage } from "../client"
import { keyHelpText } from "../commands/help"
import type { View } from "../instructions"
import { mergeTranscript } from "./overlay"
import { promptQueue, waitingKind } from "./prompts"
import { forkSourceText } from "./revert"
import type { AppState } from "./store"

export function modelReference(session: SessionInfo): string {
  const model = session.model
  return model?.providerId && model.modelId ? `${model.providerId}/${model.modelId}` : ""
}

/** The open session's model catalog row (`ModelSummary.id` is `providerId/modelId`), or `undefined` when it is not in the catalog (unknown capabilities, so nothing is refused on its account). */
export function currentModel(state: Pick<AppState, "selected" | "models">): ModelSummary | undefined {
  const session = state.selected
  if (!session) return undefined
  const ref = modelReference(session)
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

export function headerText(state: AppState, server: string): string {
  if (!state.ready) return "hya · connecting…"
  const selected = state.selected
  return `hya ${selected ? `· ${selected.title || selected.id} · ${selected.agent} ${modelReference(selected)}` : "· no session"} · ${server}`
}

export interface SessionRow {
  session: SessionInfo
  /** 0 for a top-level session, 1 for its subagents, 2 for theirs. */
  depth: number
}

/**
 * Sessions as a tree, in list order: each top-level session followed by its
 * subagent sessions (`SessionInfo.parent`), depth first. A child whose parent
 * is not listed counts as top-level. `/open <number>` counts in this order.
 */
export function sessionTree(sessions: readonly SessionInfo[]): SessionRow[] {
  const ids = new Set(sessions.map((session) => session.id))
  const children = new Map<string, SessionInfo[]>()
  for (const session of sessions) {
    if (session.parent && ids.has(session.parent) && session.parent !== session.id) {
      children.set(session.parent, [...(children.get(session.parent) ?? []), session])
    }
  }
  const rows: SessionRow[] = []
  const seen = new Set<string>()
  const visit = (session: SessionInfo, depth: number): void => {
    if (seen.has(session.id)) return
    seen.add(session.id)
    rows.push({ session, depth })
    for (const child of children.get(session.id) ?? []) visit(child, depth + 1)
  }
  for (const session of sessions) if (!(session.parent && ids.has(session.parent))) visit(session, 0)
  for (const session of sessions) visit(session, 0)
  return rows
}

/**
 * The sidebar's session list; `width` cuts each line to the sidebar. A
 * top-level session is two lines (title, agent) with a blank line between
 * groups; a subagent session is one indented `↳ N. agent` line under it.
 */
export function sessionListText(state: AppState, width?: number): string {
  if (!state.ready) return "Loading…"
  if (!state.sessions.length) return "No sessions. Type a prompt or /new."
  const groups: string[][] = []
  sessionTree(state.sessions).forEach(({ session, depth }, index) => {
    const mark = session.id === state.selected?.id ? "▸" : " "
    // A pending ask outranks `running`: the session is blocked on the user.
    const running = waitingKind(state.interactions, session.id) ? " · ◌ waiting" : session.busy ? " · running" : ""
    if (depth === 0) {
      groups.push([
        truncate(`${mark} ${index + 1}. ${session.title || session.id}`, width),
        truncate(`   ${session.agent}${running}`, width),
      ])
    } else {
      groups.at(-1)!.push(truncate(`${mark}  ${"  ".repeat(depth - 1)}↳ ${index + 1}. ${session.title || session.agent}${running}`, width))
    }
  })
  return groups.map((lines) => lines.join("\n")).join("\n\n")
}

/**
 * One line per pending interaction the prompt does not show (asks of other
 * session trees): `! title · session · id` for permissions, `? title ·
 * session · id` for questions, where `session` is `askSessionLabel` (the
 * session is omitted when the ask names none).
 */
export function pendingLines(state: AppState, width?: number): string[] {
  const prompted = new Set(promptQueue(state.interactions, state).map((item) => item.id))
  return state.interactions.filter((item) => !prompted.has(item.id)).map((item) => {
    const session = item.session ? ` · ${askSessionLabel(item.session, state.sessions)}` : ""
    return truncate(`${item.type?.includes("QUESTION") ? "?" : "!"} ${item.title}${session} · ${item.id}`, width)
  })
}

/** The session list number `/open <n>` takes (sidebar order), or `undefined` when the list does not have it. */
function sessionNumber(sessionId: string, sessions: readonly SessionInfo[]): number | undefined {
  const index = sessionTree(sessions).findIndex((row) => row.session.id === sessionId)
  return index < 0 ? undefined : index + 1
}

/** Which session an ask belongs to: `<n>. <title>` (its `/open` number), or its id when the session list does not have it. */
export function askSessionLabel(sessionId: string, sessions: readonly SessionInfo[]): string {
  const number = sessionNumber(sessionId, sessions)
  if (number === undefined) return sessionId
  const session = sessions.find((row) => row.id === sessionId)!
  return `${number}. ${session.title || session.id}`
}

/** Status line when an ask arrives for a session this TUI does not have open: which session, and how to go answer it. */
export function otherAskNotice(interaction: Interaction, sessions: readonly SessionInfo[]): string {
  const sessionId = interaction.session ?? ""
  const kind = interaction.type?.includes("QUESTION") ? "Question" : "Permission needed"
  const target = sessionNumber(sessionId, sessions) ?? sessionId
  return `${kind} in ${askSessionLabel(sessionId, sessions)} · /open ${target} to answer there`
}

/** The sidebar's context box: the open session, its agent and model, message count, context occupancy and session tokens (when known), directory, server. */
export function contextText(state: AppState, server: string, width = 30): string {
  const session = state.selected
  const row = (label: string, value: string) => `${label.padEnd(9)}${truncateStart(value, Math.max(4, width - 9))}`
  const host = server.replace(/^https?:\/\//, "").replace(/\/$/, "")
  if (!session) return [row("Session", "none"), row("Server", host), ...webRows(state, row)].join("\n")
  // The merged transcript (projection + streaming overlay), not the raw
  // projection: a fresh turn's messages exist only in the overlay until the
  // next projection read, so `state.messages.length` alone under-counts.
  const messageCount = mergeTranscript(state.messages, state.overlay).length
  const usage = contextUsage(state)
  const tokens = sessionTokens(session.usage)
  const forked = forkSourceText(session.forkedFrom, state.sessions)
  return [
    row("Session", session.title || session.id),
    // The source's name is cut at its end (a title reads from the start), unlike the path rows.
    ...(forked ? [`${"Forked".padEnd(9)}${truncate(forked.replace(/^forked /, ""), Math.max(4, width - 9))}`] : []),
    row("Agent", session.agent),
    row("Model", modelReference(session) || "default"),
    row("Messages", String(messageCount)),
    ...(usage ? [row("Context", `${usage.percent}% · ${formatTokens(usage.tokens)}/${formatTokens(usage.limit)}`)] : []),
    ...(tokens !== undefined ? [row("Tokens", formatTokens(tokens))] : []),
    row("Dir", session.workdir),
    row("Server", host),
    ...webRows(state, row),
  ].join("\n")
}

/** The context box's `WebUI` row: the address without the scheme, or `unavailable`. */
function webRows(state: AppState, row: (label: string, value: string) => string): string[] {
  const web = state.web
  if (!web) return []
  return [row("WebUI", web.url ? web.url.replace(/^https?:\/\//, "").replace(/\/$/, "") : "unavailable")]
}

/** `WebUI http://127.0.0.1:3250` (status bar), or `WebUI unavailable`; `undefined` without a WebUI. */
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

/** `Todos <completed>/<total>`, the sidebar's compact form when it is hidden; `undefined` with no todos. */
export function todosCompactText(items: readonly TodoItem[]): string | undefined {
  if (!items.length) return undefined
  const completed = items.filter((item) => todoStatusText(item.status) === "completed").length
  return `Todos ${completed}/${items.length}`
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

/** Status bar fields (E22); `statusBarText` renders them with graceful truncation at `width`. */
export interface StatusBarFields {
  /** Permission mode label (state/modes.ts `modeDisplay`: `manual`, `⚠ yolo`, or a bundle mode's title); StatusBar colors it. */
  mode: string
  /** Context occupancy percent (`contextUsage`); omitted when unknown. */
  context?: number
  /** Session token total, formatted (`12.3k tok`); omitted when unknown. */
  tokens?: string
  directory: string
  /** Current git branch; "" when unknown or not a repository. */
  branch: string
  /** Compact todo count (`Todos n/m`) shown only while the sidebar is hidden. */
  todos?: string
  connected: boolean
  /** The backend was stopped on purpose (`hya serve stop`; app/reconnect.ts): `backend stopped` in the error color instead of `reconnecting`. */
  stopped?: boolean
  /** The WebUI bare `hya` serves (`WebUI <url>`), or `WebUI unavailable` in the warning color. */
  web?: WebInfo
  /** Vim mode is on: the composer's mode and a half-typed command (`2d`), shown first. */
  vim?: { mode: "insert" | "normal"; pending: string }
}

export type StatusTone = "muted" | "mode" | "accent" | "warning" | "error"

export interface StatusSegment {
  text: string
  tone: StatusTone
}

/** Context percent from which the status bar warns (warning color) and alarms (error color). */
export const contextWarnPercent = 80
export const contextAlarmPercent = 95

/**
 * The status bar's segments in order: with vim mode on `-- INSERT --` /
 * `-- NORMAL --` (plus a pending command, `-- NORMAL -- 2d`), `mode <mode>`, `ctx N%`, `<n> tok`,
 * the directory, `⎇ <branch>`, `WebUI <url>` (or `WebUI unavailable`),
 * `Todos n/m`, `reconnecting` (or `backend stopped`). Segments with no
 * data are omitted; the least essential (from the end) drop first so the
 * line fits `width`.
 */
export function statusBarSegments(fields: StatusBarFields, width: number): StatusSegment[] {
  const context = fields.context
  const vim = fields.vim
  const segments: (StatusSegment | undefined)[] = [
    vim ? { text: `-- ${vim.mode === "normal" ? "NORMAL" : "INSERT"} --${vim.pending ? ` ${vim.pending}` : ""}`, tone: vim.mode === "normal" ? "accent" : "muted" } : undefined,
    { text: `mode ${fields.mode}`, tone: "mode" },
    context !== undefined ? { text: `ctx ${context}%`, tone: context >= contextAlarmPercent ? "error" : context >= contextWarnPercent ? "warning" : "muted" } : undefined,
    fields.tokens ? { text: fields.tokens, tone: "muted" } : undefined,
    fields.directory ? { text: truncateStart(fields.directory, 24), tone: "muted" } : undefined,
    fields.branch ? { text: `⎇ ${fields.branch}`, tone: "muted" } : undefined,
    fields.web ? { text: webLabel(fields.web)!, tone: fields.web.url ? "muted" : "warning" } : undefined,
    fields.todos ? { text: fields.todos, tone: "muted" } : undefined,
    fields.stopped ? { text: "backend stopped", tone: "error" } : fields.connected ? undefined : { text: "reconnecting", tone: "warning" },
  ]
  const shown = segments.filter((segment): segment is StatusSegment => Boolean(segment))
  const keep = vim ? 2 : 1
  while (shown.length > keep && shown.map((segment) => segment.text).join(" · ").length > width) shown.pop()
  return shown
}

/** The status bar as one line (`statusBarSegments` joined with ` · `, clipped to `width`). */
export function statusBarText(fields: StatusBarFields, width: number): string {
  return truncate(statusBarSegments(fields, width).map((segment) => segment.text).join(" · "), width)
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

export function mainContent(state: AppState): string {
  if (!state.ready) return ""
  switch (state.view) {
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
