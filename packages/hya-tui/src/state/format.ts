/** Pure text for each panel, derived from the store. Components only lay it out. */
import type { MessageInfo, SessionInfo } from "../client"
import { helpText } from "../commands/help"
import type { View } from "../instructions"
import { mergeTranscript } from "./overlay"
import type { AppState } from "./store"

export function modelReference(session: SessionInfo): string {
  const model = session.model
  return model?.providerId && model.modelId ? `${model.providerId}/${model.modelId}` : ""
}

export function formatMessage(message: MessageInfo): string {
  const cached = formatted.get(message)
  if (cached !== undefined) return cached
  const text = formatUncached(message)
  formatted.set(message, text)
  return text
}

/**
 * Formatted text per message object. Projection rows and unchanged overlay
 * snapshots keep their identity between renders, so a streaming delta only
 * re-formats the message it touched.
 */
const formatted = new WeakMap<MessageInfo, string>()

function formatUncached(message: MessageInfo): string {
  const role = message.role.replace(/^ROLE_/, "").toLowerCase()
  const lines = (message.parts ?? []).map((part) => {
    if (part.text) return part.text.text
    if (part.toolCall) return `↳ ${part.toolCall.tool}  ${part.toolCall.state ?? ""}`
    if (part.toolResult) return `  ${part.toolResult.errorMessage ?? part.toolResult.output}`
    if (part.attachment) return `  attachment: ${part.attachment.name}`
    return ""
  }).filter(Boolean)
  if (message.error) lines.push(`error · ${message.error.code ? `${message.error.code}: ` : ""}${message.error.message}`)
  return `${role}${message.finish ? ` · ${message.finish.replace(/^FINISH_REASON_/, "").toLowerCase()}` : ""}\n${lines.join("\n") || "…"}`
}

/** The transcript shown in the chat view: the projection with the streaming overlay folded in. */
export function transcript(state: AppState): MessageInfo[] {
  return mergeTranscript(state.messages, state.overlay)
}

/** Prompts waiting for the running turn, shown dimmed below the transcript in the chat view. */
export function queuedText(state: AppState): string {
  if (state.view !== "chat") return ""
  return state.queued
    .filter((item) => item.state === "queued")
    .map((item) => `user · queued\n${item.text}`)
    .join("\n\n")
}

export function headerText(state: AppState, server: string): string {
  if (!state.ready) return "hya · connecting…"
  const selected = state.selected
  return `hya ${selected ? `· ${selected.title || selected.id} · ${selected.agent} ${modelReference(selected)}` : "· no session"} · ${server}`
}

export function sessionListText(state: AppState): string {
  if (!state.ready) return "Loading…"
  return state.sessions.length
    ? state.sessions.map((session, index) => `${session.id === state.selected?.id ? "▸" : " "} ${index + 1}. ${session.title || session.id}\n   ${session.agent}${session.busy ? " · running" : ""}`).join("\n\n")
    : "No sessions. Type a prompt or /new."
}

export function pendingText(state: AppState): string {
  if (!state.ready) return ""
  return state.interactions.length
    ? state.interactions.map((item) => `${item.type?.includes("QUESTION") ? "?" : "!"} ${item.title}\n${item.id}`).join("\n\n")
    : "No pending requests"
}

const titles: Record<View, string> = {
  chat: "Chat", models: "Models", workflows: "Workflows", interactions: "Interactions",
  keys: "Saved provider keys", api: "API commands", help: "Help",
}

export function mainTitle(view: View): string { return titles[view] }

export function mainContent(state: AppState): string {
  if (!state.ready) return ""
  switch (state.view) {
    case "chat": {
      const messages = transcript(state)
      if (messages.length) return messages.slice(-50).map(formatMessage).join("\n\n")
      return state.queued.length ? "" : "No messages yet. Type a prompt below."
    }
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
