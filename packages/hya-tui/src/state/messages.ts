/**
 * The transcript view model: each message (projection merged with the
 * streaming overlay, plus the waiting queued prompts) becomes a `MessageView`
 * with a role, the assistant's agent and model, typed blocks, and at most one
 * finish notice. Components render these; they never read `MessageInfo`.
 *
 * Views are cached per `MessageInfo` object. Projected messages and unchanged
 * overlay messages keep their identity between store updates, so a streaming
 * delta rebuilds only the view of the message it touched.
 */
import type { MessageInfo, MessagePart } from "../client"
import { modelReference } from "./format"
import { mergeTranscript } from "./overlay"
import type { AppState, QueuedPrompt } from "./store"

export type Role = "user" | "assistant" | "system" | "tool" | "unknown"

export type Block =
  | { kind: "text"; id: string; text: string }
  | { kind: "reasoning"; id: string; text: string; words: number; active: boolean }
  | { kind: "tool"; id: string; tool: string; state: string; error?: string }
  | { kind: "attachment"; id: string; name: string }

export interface Notice {
  kind: "error" | "cancelled" | "length"
  text: string
}

export interface MessageView {
  id: string
  role: Role
  /** Assistant header: agent name and `provider/model`. */
  agent: string
  model: string
  blocks: Block[]
  notice?: Notice
  /** An assistant message without a finish yet. */
  streaming: boolean
  /** A prompt waiting in the client-side queue. */
  queued: boolean
}

export interface Attribution {
  agent: string
  model: string
}

/** Most messages rendered in the transcript (oldest dropped first). */
export const transcriptLimit = 200

const roles: Record<string, Role> = {
  ROLE_USER: "user", ROLE_ASSISTANT: "assistant", ROLE_SYSTEM: "system", ROLE_TOOL: "tool",
}

function words(text: string): number {
  return text.split(/\s+/).filter(Boolean).length
}

/** `TOOL_EXECUTION_STATE_OK` → `ok`. */
function toolState(state: string | undefined): string {
  return (state ?? "").replace(/^[A-Z_]*STATE_/, "").toLowerCase()
}

function block(part: MessagePart, active: boolean): Block | undefined {
  if (part.text) return part.text.text ? { kind: "text", id: part.id, text: part.text.text } : undefined
  if (part.reasoning) return { kind: "reasoning", id: part.id, text: part.reasoning.text, words: words(part.reasoning.text), active }
  if (part.toolCall) {
    const error = part.toolCall.errorMessage
    return { kind: "tool", id: part.id, tool: part.toolCall.tool, state: toolState(part.toolCall.state), ...(error ? { error } : {}) }
  }
  if (part.toolResult) {
    return { kind: "tool", id: part.id, tool: "result", state: part.toolResult.errorMessage ? "error" : "ok", ...(part.toolResult.errorMessage ? { error: part.toolResult.errorMessage } : {}) }
  }
  if (part.attachment) return { kind: "attachment", id: part.id, name: part.attachment.name }
  return undefined
}

/** The one notice worth showing for a message's end: an error, a cancel, or the length limit. */
export function finishNotice(message: MessageInfo): Notice | undefined {
  if (message.error) {
    const { code, message: text } = message.error
    return { kind: "error", text: `✗ ${code ? `${code}: ` : ""}${text}` }
  }
  switch (message.finish) {
    case "FINISH_REASON_ERROR": return { kind: "error", text: "✗ Turn failed" }
    case "FINISH_REASON_CANCELLED": return { kind: "cancelled", text: "! Cancelled" }
    case "FINISH_REASON_LENGTH": return { kind: "length", text: "! Reply stopped at the output length limit" }
    default: return undefined
  }
}

function build(message: MessageInfo, fallback: Attribution): MessageView {
  const role = roles[message.role] ?? "unknown"
  const streaming = role === "assistant" && !message.finish
  const parts = message.parts ?? []
  const blocks = parts
    .map((part, index) => block(part, streaming && index === parts.length - 1))
    .filter((item): item is Block => item !== undefined)
  const notice = finishNotice(message)
  return {
    id: message.id,
    role,
    agent: message.agent || fallback.agent,
    model: message.model || fallback.model,
    blocks,
    ...(notice ? { notice } : {}),
    streaming,
    queued: false,
  }
}

const views = new WeakMap<MessageInfo, { key: string; view: MessageView }>()

export function messageView(message: MessageInfo, fallback: Attribution): MessageView {
  const key = `${fallback.agent}\u0000${fallback.model}`
  const cached = views.get(message)
  if (cached?.key === key) return cached.view
  const view = build(message, fallback)
  views.set(message, { key, view })
  return view
}

const queuedViews = new WeakMap<QueuedPrompt, MessageView>()

export function queuedView(item: QueuedPrompt): MessageView {
  let view = queuedViews.get(item)
  if (!view) {
    const id = `queued-${item.id}`
    view = { id, role: "user", agent: "", model: "", blocks: [{ kind: "text", id, text: item.text }], streaming: false, queued: true }
    queuedViews.set(item, view)
  }
  return view
}

/** The chat transcript: projection + overlay, then prompts still waiting in the queue. */
export function transcriptViews(state: AppState): MessageView[] {
  const session = state.selected
  const fallback = { agent: session?.agent ?? "", model: session ? modelReference(session) : "" }
  const messages = mergeTranscript(state.messages, state.overlay).slice(-transcriptLimit)
  return [
    ...messages.map((message) => messageView(message, fallback)),
    ...state.queued.filter((item) => item.state === "queued").map(queuedView),
  ]
}

/** Whether a reasoning part is expanded: its own toggle, else the global `/thinking` switch. */
export function reasoningExpanded(state: Pick<AppState, "thinking" | "reasoningToggles">, partId: string): boolean {
  return state.reasoningToggles.get(partId) ?? state.thinking
}

/** The one-line summary of a reasoning block, e.g. `▸ Thinking · 42 words`. */
export function reasoningLabel(block: Extract<Block, { kind: "reasoning" }>, expanded: boolean): string {
  return `${expanded ? "▾" : "▸"} Thinking${block.active ? "…" : ""} · ${block.words} word${block.words === 1 ? "" : "s"}`
}
