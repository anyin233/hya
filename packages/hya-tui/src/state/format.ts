/** Pure text for the header, sidebar, pending block, and non-chat views, derived from the store. */
import type { SessionInfo } from "../client"
import { helpText } from "../commands/help"
import type { View } from "../instructions"
import { promptQueue, waitingKind } from "./prompts"
import type { AppState } from "./store"

export function modelReference(session: SessionInfo): string {
  const model = session.model
  return model?.providerId && model.modelId ? `${model.providerId}/${model.modelId}` : ""
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
 * session trees): `! title · id` for permissions, `? title · id` for questions.
 */
export function pendingLines(state: AppState, width?: number): string[] {
  const prompted = new Set(promptQueue(state.interactions, state).map((item) => item.id))
  return state.interactions.filter((item) => !prompted.has(item.id)).map((item) => truncate(`${item.type?.includes("QUESTION") ? "?" : "!"} ${item.title} · ${item.id}`, width))
}

/** The sidebar's context box: the open session, its agent and model, message count, directory, server. */
export function contextText(state: AppState, server: string, width = 30): string {
  const session = state.selected
  const row = (label: string, value: string) => `${label.padEnd(9)}${truncateStart(value, Math.max(4, width - 9))}`
  const host = server.replace(/^https?:\/\//, "").replace(/\/$/, "")
  if (!session) return [row("Session", "none"), row("Server", host)].join("\n")
  return [
    row("Session", session.title || session.id),
    row("Agent", session.agent),
    row("Model", modelReference(session) || "default"),
    row("Messages", String(state.messages.length)),
    row("Dir", session.workdir),
    row("Server", host),
  ].join("\n")
}

const titles: Record<View, string> = {
  chat: "Chat", models: "Models", workflows: "Workflows", interactions: "Interactions",
  keys: "Saved provider keys", api: "API commands", help: "Help", todos: "Todos", status: "Status",
}

/** `TODO_STATUS_IN_PROGRESS` → `in_progress`, etc. */
function todoStatusText(status: string): string {
  return status.replace(/^[A-Z_]*STATUS_/, "").toLowerCase() || "pending"
}

const todoGlyphs: Record<string, string> = {
  pending: "☐", in_progress: "▸", blocked: "!", completed: "✓",
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
    case "keys": {
      const ids = [...new Set([...state.providers.map((provider) => provider.id), ...state.savedKeys])].sort()
      return !state.savedKeysAvailable
        ? "Key listing is unavailable on this backend. Restart with hya 0.41.0 or newer."
        : ids.length
        ? ids.map((id) => `${state.savedKeys.includes(id) ? "● saved" : "○ no saved key"}  ${id}`).join("\n")
        : "No providers or saved keys. Use /key set <provider> to add one."
    }
    case "api": return state.apiOutput
    case "help": return helpText
    case "todos": return todosText(state.todos)
    case "status": return state.statusText
  }
}
