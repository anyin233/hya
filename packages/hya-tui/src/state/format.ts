/** Pure text for the header, sidebar, pending block, and non-chat views, derived from the store. */
import type { SessionInfo } from "../client"
import { helpText } from "../commands/help"
import type { View } from "../instructions"
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

/** The sidebar's session list; `width` cuts each line to the sidebar. */
export function sessionListText(state: AppState, width?: number): string {
  if (!state.ready) return "Loading…"
  return state.sessions.length
    ? state.sessions.map((session, index) => [
      truncate(`${session.id === state.selected?.id ? "▸" : " "} ${index + 1}. ${session.title || session.id}`, width),
      truncate(`   ${session.agent}${session.busy ? " · running" : ""}`, width),
    ].join("\n")).join("\n\n")
    : "No sessions. Type a prompt or /new."
}

/** One line per pending interaction: `! title · id` for permissions, `? title · id` for questions. */
export function pendingLines(state: AppState, width?: number): string[] {
  return state.interactions.map((item) => truncate(`${item.type?.includes("QUESTION") ? "?" : "!"} ${item.title} · ${item.id}`, width))
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
  keys: "Saved provider keys", api: "API commands", help: "Help",
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
  }
}
