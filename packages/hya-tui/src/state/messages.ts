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
  /** `command` / `output`: a shell command and its output text, when known (the part's input/output JSON or this TUI's own shell turn). */
  | { kind: "tool"; id: string; tool: string; state: string; error?: string; command?: string; output?: string }
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

/** Longest tool output shown under a tool line, in lines. */
export const toolOutputLines = 12

function parseJson(text: string | undefined): unknown {
  if (!text) return undefined
  try {
    return JSON.parse(text) as unknown
  } catch {
    return text
  }
}

/** The `command` string of a tool input (the shell tool's argument), if any. */
function inputCommand(inputJson: string | undefined): string | undefined {
  const input = parseJson(inputJson)
  if (input && typeof input === "object" && typeof (input as { command?: unknown }).command === "string") {
    return (input as { command: string }).command
  }
  return undefined
}

/**
 * Readable text of a tool's output JSON: a JSON string as is, else its
 * `output`, `stdout`, `text`, or `content` string field, else pretty JSON
 * (non-JSON text as is). Trailing white space is dropped and more than
 * `toolOutputLines` lines are cut with a `… N more lines` line.
 */
export function toolOutputText(outputJson: string | undefined): string | undefined {
  const value = parseJson(outputJson)
  if (value === undefined || value === null) return undefined
  let text: string
  if (typeof value === "string") text = value
  else {
    const record = value as Record<string, unknown>
    const field = ["output", "stdout", "text", "content"].map((name) => record[name]).find((item) => typeof item === "string")
    text = typeof field === "string" ? field : JSON.stringify(value, null, 2)
  }
  const lines = text.replace(/\s+$/, "").split("\n")
  if (!lines.join("")) return undefined
  if (lines.length <= toolOutputLines) return lines.join("\n")
  return [...lines.slice(0, toolOutputLines), `… ${lines.length - toolOutputLines} more lines`].join("\n")
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
    const command = inputCommand(part.toolCall.inputJson)
    const output = toolOutputText(part.toolCall.outputJson)
    return {
      kind: "tool", id: part.id, tool: part.toolCall.tool, state: toolState(part.toolCall.state),
      ...(error ? { error } : {}), ...(command !== undefined ? { command } : {}), ...(output !== undefined ? { output } : {}),
    }
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

function build(message: MessageInfo, fallback: Attribution, shell: string | undefined): MessageView {
  const role = roles[message.role] ?? "unknown"
  const streaming = role === "assistant" && !message.finish
  const parts = message.parts ?? []
  let blocks = parts
    .map((part, index) => block(part, streaming && index === parts.length - 1))
    .filter((item): item is Block => item !== undefined)
  // A shell turn run from this TUI: its command is known even when the part has no input.
  if (shell !== undefined) blocks = blocks.map((item) => item.kind === "tool" && item.command === undefined ? { ...item, command: shell } : item)
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

/** `shell`: the command of this TUI's shell turn whose assistant message this is. */
export function messageView(message: MessageInfo, fallback: Attribution, shell?: string): MessageView {
  const key = `${fallback.agent}\u0000${fallback.model}\u0000${shell ?? "\u0001"}`
  const cached = views.get(message)
  if (cached?.key === key) return cached.view
  const view = build(message, fallback, shell)
  views.set(message, { key, view })
  return view
}

/** The command of a shell turn's assistant view: its first tool block's command. */
function shellCommandOf(view: MessageView | undefined): string | undefined {
  if (view?.role !== "assistant") return undefined
  const tool = view.blocks.find((item) => item.kind === "tool")
  return tool?.kind === "tool" ? tool.command : undefined
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

/** The chat transcript: projection + overlay, then prompts still waiting in the queue. */
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
  const views = messages.map((message) => messageView(message, fallback, commands.get(message.id)))
  // The user message of a shell turn shows the command the user typed.
  const shown = views.map((view, index) => {
    if (view.role !== "user") return view
    const next = messages[index + 1]
    const known = index === pendingUser ? state.pendingShell : next ? commands.get(next.id) : undefined
    const text = view.blocks.length === 1 && view.blocks[0]!.kind === "text" ? view.blocks[0]!.text : undefined
    const command = known ?? (text === shellMarker ? shellCommandOf(views[index + 1]) : undefined)
    return command === undefined ? view : shellUserView(view, command)
  })
  return [...shown, ...state.queued.filter((item) => item.state === "queued").map(queuedView)]
}

/** Whether a reasoning part is expanded: its own toggle, else the global `/thinking` switch. */
export function reasoningExpanded(state: Pick<AppState, "thinking" | "reasoningToggles">, partId: string): boolean {
  return state.reasoningToggles.get(partId) ?? state.thinking
}

/** The one-line summary of a reasoning block, e.g. `▸ Thinking · 42 words`. */
export function reasoningLabel(block: Extract<Block, { kind: "reasoning" }>, expanded: boolean): string {
  return `${expanded ? "▾" : "▸"} Thinking${block.active ? "…" : ""} · ${block.words} word${block.words === 1 ? "" : "s"}`
}
