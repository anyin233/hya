/**
 * The transient streaming overlay: stream frames folded by message and part
 * id, shown on top of the server projection until the projection catches up.
 *
 * The projection (`ListMessages`) stays authoritative. The overlay only
 * covers what the projection cannot show yet — above all the live assistant
 * text of the in-flight round, which is not in the projection until the
 * round's durable `partReplaced`. Fold rules (docs/protocol/README.md "Live
 * and durable frames"):
 *
 * - Durable frames carry `seq`; one at or below the last applied seq is a
 *   duplicate (reconnect / gap-fill overlap) and is ignored. Live frames have
 *   no `seq` and are always applied.
 * - `partStarted` for a known part id is not a new part (live + durable start).
 * - `partAppended` appends `textDelta`; `partReplaced` sets the whole text.
 * - After a `resync` (`markLiveLost`), live deltas of parts that were already
 *   streaming are dropped until the durable `partReplaced` sets their text,
 *   so a gap never garbles the text.
 *
 * Pure TypeScript (no Solid): the store wraps it and publishes snapshots.
 */
import type { MessageError, MessageInfo, MessagePart, StreamEvent } from "../client"

interface OverlayPart {
  id: string
  kind: "text" | "reasoning"
  text: string
  /** Text was set by a durable frame; later live deltas are late duplicates. */
  durable: boolean
  /** Live deltas were lost (resync) before the durable text arrived. */
  liveLost: boolean
}

interface OverlayMessage {
  id: string
  role: string | undefined
  finish?: string
  cause?: string
  error?: MessageError
  parts: OverlayPart[]
  /** Cached snapshot; cleared whenever the message changes. */
  view?: MessageInfo
}

/** A message's terminal frame, as reported by `apply`. */
export interface FinishedInfo {
  message: string
  role: string | undefined
  finish: string | undefined
  cause?: string
}

export interface OverlayEffect {
  /** The overlay's visible content changed (a flush is due). */
  changed: boolean
  /** A durable frame newer than the last applied seq was applied. */
  durable: boolean
  /** Set for `messageFinished`. */
  finished?: FinishedInfo
}

const none: OverlayEffect = { changed: false, durable: false }

export const assistantRole = "ROLE_ASSISTANT"
export const toolCallsFinish = "FINISH_REASON_TOOL_CALLS"

export class TranscriptOverlay {
  private seq: bigint
  private readonly byId = new Map<string, OverlayMessage>()

  constructor(lastSeq = "0") {
    this.seq = BigInt(lastSeq || "0")
  }

  /** Last applied durable sequence, as a decimal string (64-bit safe). */
  get lastSeq(): string { return this.seq.toString() }

  /** Forget everything (session switch) and continue after `lastSeq`. */
  reset(lastSeq = "0"): void {
    this.byId.clear()
    this.seq = BigInt(lastSeq || "0")
  }

  /** Whether the overlay has seen `messageStarted` (or any frame) for `id`. */
  knows(id: string): boolean { return this.byId.has(id) }

  role(id: string): string | undefined { return this.byId.get(id)?.role }

  /** Fold one stream event. */
  apply(event: StreamEvent): OverlayEffect {
    const durable = Boolean(event.seq && event.seq !== "0")
    if (durable) {
      const seq = BigInt(event.seq!)
      if (seq <= this.seq) return none
      this.seq = seq
    }
    const base: OverlayEffect = { changed: false, durable }
    if (event.messageStarted) {
      const message = this.message(event.messageStarted.message, event.messageStarted.role)
      if (event.messageStarted.role) message.role = event.messageStarted.role
      return { ...base, changed: true }
    }
    if (event.messageFinished) {
      const { message: id, finish, cause } = event.messageFinished
      const message = this.byId.get(id)
      if (message) {
        message.finish = finish
        message.cause = cause
        message.view = undefined
      }
      return {
        ...base,
        changed: Boolean(message),
        finished: { message: id, role: message?.role, finish, ...(cause ? { cause } : {}) },
      }
    }
    if (event.partStarted) {
      const { message: id, part: partId, kind } = event.partStarted
      if (kind !== "text" && kind !== "reasoning") return base
      const message = this.message(id, durable ? undefined : assistantRole)
      if (message.parts.some((part) => part.id === partId)) return base
      message.parts.push({ id: partId, kind, text: "", durable: false, liveLost: false })
      message.view = undefined
      return { ...base, changed: true }
    }
    if (event.partAppended) {
      const { message: id, part: partId, textDelta } = event.partAppended
      const found = this.part(id, partId)
      if (!found || !textDelta) return base
      if (!durable && (found.part.durable || found.part.liveLost)) return base
      found.part.text += textDelta
      found.message.view = undefined
      return { ...base, changed: true }
    }
    if (event.partReplaced) {
      const { message: id, part: partId, text } = event.partReplaced
      const found = this.part(id, partId)
      if (!found) return base
      found.part.text = text ?? ""
      if (durable) {
        found.part.durable = true
        found.part.liveLost = false
      }
      found.message.view = undefined
      return { ...base, changed: true }
    }
    if (event.errorReported?.message) {
      const { message: id, code, errorMessage } = event.errorReported
      const message = this.message(id, assistantRole)
      message.error = { code: code ?? "", message: errorMessage ?? "" }
      message.view = undefined
      return { ...base, changed: true }
    }
    return base
  }

  /** A `resync` lost frames: stop live deltas of parts that were mid-stream. */
  markLiveLost(): void {
    for (const message of this.byId.values()) {
      for (const part of message.parts) if (!part.durable) part.liveLost = true
    }
  }

  /**
   * The assistant message that ended the turn started by user message
   * `userMessage`: the first later assistant `messageFinished` whose finish is
   * not `FINISH_REASON_TOOL_CALLS` (a tool-call round continues the turn).
   */
  turnEnd(userMessage: string): FinishedInfo | undefined {
    let after = false
    for (const message of this.byId.values()) {
      if (message.id === userMessage) {
        after = true
        continue
      }
      if (after && message.role === assistantRole && message.finish && message.finish !== toolCallsFinish) {
        return { message: message.id, role: message.role, finish: message.finish, ...(message.cause ? { cause: message.cause } : {}) }
      }
    }
    return undefined
  }

  error(id: string): MessageError | undefined { return this.byId.get(id)?.error }

  /** Drop messages the projection already shows finished; it is authoritative for them. */
  prune(projection: MessageInfo[]): void {
    for (const message of projection) if (message.finish) this.byId.delete(message.id)
  }

  /** Snapshot in arrival order. Unchanged messages keep their object identity. */
  messages(): MessageInfo[] {
    return [...this.byId.values()].map((message) => (message.view ??= toMessageInfo(message)))
  }

  private message(id: string, role: string | undefined): OverlayMessage {
    let message = this.byId.get(id)
    if (!message) {
      message = { id, role, parts: [] }
      this.byId.set(id, message)
    }
    return message
  }

  private part(messageId: string, partId: string): { message: OverlayMessage; part: OverlayPart } | undefined {
    const message = this.byId.get(messageId)
    const part = message?.parts.find((candidate) => candidate.id === partId)
    return message && part ? { message, part } : undefined
  }
}

function toPart(part: OverlayPart): MessagePart {
  return part.kind === "text" ? { id: part.id, text: { text: part.text } } : { id: part.id, reasoning: { text: part.text } }
}

function toMessageInfo(message: OverlayMessage): MessageInfo {
  return {
    id: message.id,
    role: message.role ?? assistantRole,
    parts: message.parts.map(toPart),
    ...(message.finish ? { finish: message.finish } : {}),
    ...(message.error ? { error: message.error } : {}),
  }
}

function partText(part: MessagePart): string | undefined {
  return part.text?.text ?? part.reasoning?.text
}

/** Overlay text replaces a projected text/reasoning part of the same id. */
function withText(part: MessagePart, text: string): MessagePart {
  if (part.text) return { ...part, text: { ...part.text, text } }
  if (part.reasoning) return { ...part, reasoning: { ...part.reasoning, text } }
  return part
}

function mergeMessage(projected: MessageInfo, overlay: MessageInfo): MessageInfo {
  const overlayParts = new Map((overlay.parts ?? []).map((part) => [part.id, part]))
  const projectedIds = new Set<string>()
  const parts = (projected.parts ?? []).map((part) => {
    projectedIds.add(part.id)
    const live = overlayParts.get(part.id)
    const text = live && partText(live)
    return text !== undefined && text !== partText(part) ? withText(part, text) : part
  })
  for (const part of overlay.parts ?? []) if (!projectedIds.has(part.id)) parts.push(part)
  const finish = overlay.finish ?? projected.finish
  const error = overlay.error ?? projected.error
  return { ...projected, parts, ...(finish ? { finish } : {}), ...(error ? { error } : {}) }
}

/**
 * The transcript to display: the projection, with the overlay folded into
 * messages the projection has not finished yet, followed by overlay-only
 * messages. A projected message with a `finish` is shown exactly as projected.
 */
export function mergeTranscript(projection: MessageInfo[], overlay: MessageInfo[]): MessageInfo[] {
  if (overlay.length === 0) return projection
  const byId = new Map(overlay.map((message) => [message.id, message]))
  const merged = projection.map((message) => {
    const live = byId.get(message.id)
    if (!live) return message
    byId.delete(message.id)
    return message.finish ? message : mergeMessage(message, live)
  })
  return [...merged, ...byId.values()]
}
