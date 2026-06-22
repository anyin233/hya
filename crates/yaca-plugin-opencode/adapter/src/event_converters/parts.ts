import {
  isRecord,
  numberField,
  partEvent,
  partIds,
  stringField,
  stringifyOutput,
  toolIds,
} from "./fields"
import type { EventEnvelope, OpenCodeEvent } from "./types"

export function textPartEvent(
  envelope: EventEnvelope,
  text: string | undefined,
  delta?: string,
  ended = false,
): OpenCodeEvent | undefined {
  const ids = partIds(envelope.event)
  if (ids === undefined || text === undefined) {
    return undefined
  }
  return partEvent(envelope, {
    id: ids.part,
    sessionID: ids.session,
    messageID: ids.message,
    type: "text",
    text,
    time: partTime(envelope, ended),
  }, delta)
}

export function reasoningPartEvent(
  envelope: EventEnvelope,
  text: string | undefined,
  delta?: string,
  ended = false,
): OpenCodeEvent | undefined {
  const ids = partIds(envelope.event)
  if (ids === undefined || text === undefined) {
    return undefined
  }
  return partEvent(envelope, {
    id: ids.part,
    sessionID: ids.session,
    messageID: ids.message,
    type: "reasoning",
    text,
    time: partTime(envelope, ended),
  }, delta)
}

export function toolCallRequestedEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const ids = toolIds(envelope.event)
  const tool = stringField(envelope.event, "name")
  if (ids === undefined || tool === undefined) {
    return undefined
  }
  return partEvent(envelope, {
    id: ids.part,
    sessionID: ids.session,
    messageID: ids.message,
    type: "tool",
    callID: ids.call,
    tool,
    state: {
      status: "running",
      input: isRecord(envelope.event.input) ? envelope.event.input : {},
      time: { start: envelope.ts_millis },
    },
  })
}

export function toolInputStartEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const ids = toolIds(envelope.event)
  const tool = stringField(envelope.event, "name")
  if (ids === undefined || tool === undefined) {
    return undefined
  }
  return partEvent(envelope, {
    id: ids.part,
    sessionID: ids.session,
    messageID: ids.message,
    type: "tool",
    callID: ids.call,
    tool,
    state: {
      status: "pending",
      input: {},
      raw: "",
    },
  })
}

export function toolInputDeltaEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const ids = toolIds(envelope.event)
  const tool = stringField(envelope.event, "name")
  const delta = stringField(envelope.event, "delta")
  if (ids === undefined || tool === undefined || delta === undefined) {
    return undefined
  }
  return partEvent(envelope, {
    id: ids.part,
    sessionID: ids.session,
    messageID: ids.message,
    type: "tool",
    callID: ids.call,
    tool,
    state: {
      status: "pending",
      input: {},
      raw: delta,
    },
  }, delta)
}

export function toolResultEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const ids = toolIds(envelope.event)
  if (ids === undefined) {
    return undefined
  }
  const timeMs = numberField(envelope.event, "time_ms") ?? 0
  return partEvent(envelope, {
    id: ids.part,
    sessionID: ids.session,
    messageID: ids.message,
    type: "tool",
    callID: ids.call,
    tool: "unknown",
    state: {
      status: "completed",
      input: {},
      output: stringifyOutput(envelope.event.output),
      title: "",
      metadata: {},
      time: { start: envelope.ts_millis, end: envelope.ts_millis + timeMs },
    },
  })
}

export function toolErrorEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const ids = toolIds(envelope.event)
  const message = stringField(envelope.event, "message_text")
  if (ids === undefined || message === undefined) {
    return undefined
  }
  return partEvent(envelope, {
    id: ids.part,
    sessionID: ids.session,
    messageID: ids.message,
    type: "tool",
    callID: ids.call,
    tool: "unknown",
    state: {
      status: "error",
      input: {},
      error: message,
      time: { start: envelope.ts_millis, end: envelope.ts_millis },
    },
  })
}

function partTime(envelope: EventEnvelope, ended: boolean): Record<string, number> {
  return ended
    ? { start: envelope.ts_millis, end: envelope.ts_millis }
    : { start: envelope.ts_millis }
}
