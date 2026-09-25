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
import { toolCard, type ToolCardView } from "./tools"

/** `divider` is synthetic (a `CompactionApplied` notice), never a server role. */
export type Role = "user" | "assistant" | "system" | "tool" | "unknown" | "divider"

export type Block =
  | { kind: "text"; id: string; text: string }
  | { kind: "reasoning"; id: string; text: string; words: number; active: boolean }
  /**
   * A tool call card (state/tools.ts). `callId` links a `task` card to its
   * member; `shell` marks the tool call of a `!command` shell turn (its card
   * starts expanded).
   */
  | { kind: "tool"; id: string; callId?: string; card: ToolCardView; shell?: boolean }
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

/**
 * The text the backend records as the user message of a shell turn
 * (`ShellTurn`); the transcript shows `!<command>` in its place when the
 * command is known.
 */
export const shellMarker = "The following tool was executed by the user"

function block(part: MessagePart, active: boolean, shell: string | undefined, shellTurn: boolean): Block | undefined {
  if (part.text) return part.text.text ? { kind: "text", id: part.id, text: part.text.text } : undefined
  if (part.reasoning) return { kind: "reasoning", id: part.id, text: part.reasoning.text, words: words(part.reasoning.text), active }
  if (part.toolCall) {
    const card = toolCard(part.toolCall, shell !== undefined ? { command: shell } : {})
    return { kind: "tool", id: part.id, ...(part.toolCall.callId ? { callId: part.toolCall.callId } : {}), card, ...(shellTurn ? { shell: true } : {}) }
  }
  if (part.toolResult) {
    // Not emitted by v1 servers (the output is on the tool call); kept for older data.
    const card = toolCard({ tool: "result", state: part.toolResult.errorMessage ? "TOOL_EXECUTION_STATE_ERROR" : "TOOL_EXECUTION_STATE_OK", outputJson: JSON.stringify(part.toolResult.output), ...(part.toolResult.errorMessage ? { errorMessage: part.toolResult.errorMessage } : {}) })
    return { kind: "tool", id: part.id, card }
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

function build(message: MessageInfo, fallback: Attribution, shell: string | undefined, shellTurn: boolean): MessageView {
  const role = roles[message.role] ?? "unknown"
  const streaming = role === "assistant" && !message.finish
  const parts = message.parts ?? []
  // A shell turn run from this TUI: its command is known even when the part has no input.
  const blocks = parts
    .map((part, index) => block(part, streaming && index === parts.length - 1, shell, shellTurn))
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

/**
 * `shell`: the command of this TUI's shell turn whose assistant message this
 * is. `shellTurn`: the message answers a shell turn's user message (its tool
 * cards start expanded); implied by `shell`.
 */
export function messageView(message: MessageInfo, fallback: Attribution, shell?: string, shellTurn = shell !== undefined): MessageView {
  const key = `${fallback.agent}\u0000${fallback.model}\u0000${shell ?? "\u0001"}\u0000${shellTurn}`
  const cached = views.get(message)
  if (cached?.key === key) return cached.view
  const view = build(message, fallback, shell, shellTurn)
  views.set(message, { key, view })
  return view
}

/** The command of a shell turn's assistant view: its first tool block's command. */
function shellCommandOf(view: MessageView | undefined): string | undefined {
  if (view?.role !== "assistant") return undefined
  const tool = view.blocks.find((item) => item.kind === "tool")
  return tool?.kind === "tool" ? tool.card.command : undefined
}

const shellUserViews = new WeakMap<MessageView, { command: string; view: MessageView }>()

/** A shell turn's user view with `!<command>` in place of the backend's marker text. */
function shellUserView(view: MessageView, command: string): MessageView {
  const cached = shellUserViews.get(view)
  if (cached?.command === command) return cached.view
  const first = view.blocks[0]
  const replaced: MessageView = { ...view, blocks: [{ kind: "text", id: first?.id ?? view.id, text: `!${command}` }] }
  shellUserViews.set(view, { command, view: replaced })
  return replaced
}

const commandUserViews = new WeakMap<MessageView, { text: string; view: MessageView }>()

/** A command turn's user view with the `/name args` the user typed in place of the backend's expanded template. */
function commandUserView(view: MessageView, text: string): MessageView {
  const cached = commandUserViews.get(view)
  if (cached?.text === text) return cached.view
  const first = view.blocks[0]
  const replaced: MessageView = { ...view, blocks: [{ kind: "text", id: first?.id ?? view.id, text }] }
  commandUserViews.set(view, { text, view: replaced })
  return replaced
}

const queuedViews = new WeakMap<QueuedPrompt, MessageView>()

export function queuedView(item: QueuedPrompt): MessageView {
  let view = queuedViews.get(item)
  if (!view) {
    const id = `queued-${item.id}`
    view = { id, role: "user", agent: "", model: "", blocks: [{ kind: "text", id, text: item.shell ? `!${item.text}` : item.text }], streaming: false, queued: true }
    queuedViews.set(item, view)
  }
  return view
}

/** A `CompactionApplied` divider (state/store.ts `Divider`) as a synthetic view. */
export function dividerView(divider: { id: string; text: string }): MessageView {
  return { id: divider.id, role: "divider", agent: "", model: "", blocks: [{ kind: "text", id: divider.id, text: divider.text }], streaming: false, queued: false }
}

/**
 * Splice compaction dividers into `views` right after the message that was
 * newest when each fired; a divider whose message fell out of the rendered
 * window (or was never seen) goes at the end, just before queued prompts.
 */
function withDividers(views: MessageView[], dividers: AppState["dividers"]): MessageView[] {
  if (!dividers.length) return views
  const result = [...views]
  for (const divider of dividers) {
    const at = divider.afterMessageId ? result.findIndex((view) => view.id === divider.afterMessageId) : -1
    const view = dividerView(divider)
    if (at >= 0) result.splice(at + 1, 0, view)
    else result.push(view)
  }
  return result
}

/** The chat transcript: projection + overlay, dividers, then prompts still waiting in the queue. */
export function transcriptViews(state: AppState): MessageView[] {
  const session = state.selected
  const fallback = { agent: session?.agent ?? "", model: session ? modelReference(session) : "" }
  const messages = mergeTranscript(state.messages, state.overlay).slice(-transcriptLimit)
  const commands = new Map(state.shellCommands)
  // The running shell turn: its marker message is the last user message, and the message after it is its reply.
  let pendingUser = -1
  if (state.pendingShell !== undefined) {
    const last = messages.findLastIndex((message) => roles[message.role] === "user")
    const text = messages[last]?.parts?.map((part) => part.text?.text ?? "").join("")
    const reply = messages[last + 1]
    if (text === shellMarker && !(reply && commands.has(reply.id))) {
      pendingUser = last
      if (reply) commands.set(reply.id, state.pendingShell)
    }
  }
  const markerAt = (index: number): boolean => {
    const message = messages[index]
    return message !== undefined && roles[message.role] === "user" && message.parts?.map((part) => part.text?.text ?? "").join("") === shellMarker
  }
  const views = messages.map((message, index) => messageView(message, fallback, commands.get(message.id), commands.has(message.id) || markerAt(index - 1)))
  // The user message of a shell turn shows the command the user typed; a
  // command turn's user message shows the `/name args` the user typed.
  const shown = views.map((view, index) => {
    if (view.role !== "user") return view
    const invocation = state.commandDisplay.get(view.id)
    if (invocation !== undefined) return commandUserView(view, invocation)
    const next = messages[index + 1]
    const known = index === pendingUser ? state.pendingShell : next ? commands.get(next.id) : undefined
    const text = view.blocks.length === 1 && view.blocks[0]!.kind === "text" ? view.blocks[0]!.text : undefined
    const command = known ?? (text === shellMarker ? shellCommandOf(views[index + 1]) : undefined)
    return command === undefined ? view : shellUserView(view, command)
  })
  return [...withDividers(shown, state.dividers), ...state.queued.filter((item) => item.state === "queued").map(queuedView)]
}

/** Whether a tool card is expanded: its own toggle, else the global `/tools` switch, else only shell-turn cards. */
export function toolExpanded(state: Pick<AppState, "tools" | "toolToggles">, block: Extract<Block, { kind: "tool" }>): boolean {
  return state.toolToggles.get(block.id) ?? state.tools ?? block.shell === true
}

/** Whether a reasoning part is expanded: its own toggle, else the global `/thinking` switch. */
export function reasoningExpanded(state: Pick<AppState, "thinking" | "reasoningToggles">, partId: string): boolean {
  return state.reasoningToggles.get(partId) ?? state.thinking
}

/** The one-line summary of a reasoning block, e.g. `▸ Thinking · 42 words`. */
export function reasoningLabel(block: Extract<Block, { kind: "reasoning" }>, expanded: boolean): string {
  return `${expanded ? "▾" : "▸"} Thinking${block.active ? "…" : ""} · ${block.words} word${block.words === 1 ? "" : "s"}`
}
