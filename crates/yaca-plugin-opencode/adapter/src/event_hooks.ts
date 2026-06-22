import type { OpenCodeHooks } from "./loader/init"

export type EventEnvelope = {
  readonly seq: number | string
  readonly ts_millis: number
  readonly event: Readonly<Record<string, unknown>> & { readonly type: string }
}

type OpenCodeEventHook = (input: { readonly event: OpenCodeEvent }) => unknown | Promise<unknown>

type OpenCodeEvent = {
  readonly id: string
  readonly type: string
  readonly properties: Readonly<Record<string, unknown>>
}

export async function runEventHooks(
  hooks: readonly OpenCodeHooks[],
  envelope: EventEnvelope,
): Promise<void> {
  const event = openCodeEventFromEnvelope(envelope)
  if (event === undefined) {
    return
  }
  for (const hook of hooks) {
    const candidate = hook.event
    if (!isEventHook(candidate)) {
      continue
    }
    try {
      await candidate({ event })
    } catch {
      continue
    }
  }
}

function openCodeEventFromEnvelope(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const event = envelope.event
  switch (event.type) {
    case "session_created":
      return sessionCreatedEvent(envelope)
    case "error":
      return errorEvent(envelope)
    case "text_delta":
      return textDeltaEvent(envelope)
    case "tool_result":
      return toolResultEvent(envelope)
    case "tool_error":
      return toolErrorEvent(envelope)
    default:
      return undefined
  }
}

function sessionCreatedEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const session = stringField(envelope.event, "session")
  const workdir = stringField(envelope.event, "workdir")
  if (session === undefined || workdir === undefined) {
    return undefined
  }
  return {
    id: String(envelope.seq),
    type: "session.created",
    properties: {
      info: {
        id: session,
        projectID: session,
        directory: workdir,
        ...optionalString("parentID", stringField(envelope.event, "parent")),
        title: "",
        version: "0",
        time: { created: envelope.ts_millis, updated: envelope.ts_millis },
      },
    },
  }
}

function errorEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const message = stringField(envelope.event, "message")
  if (message === undefined) {
    return undefined
  }
  const code = stringField(envelope.event, "code")
  return {
    id: String(envelope.seq),
    type: "session.error",
    properties: {
      ...optionalString("sessionID", stringField(envelope.event, "session")),
      error: {
        name: "UnknownError",
        data: { message: code === undefined ? message : `${code}: ${message}` },
      },
    },
  }
}

function textDeltaEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const ids = partIds(envelope.event)
  const delta = stringField(envelope.event, "delta")
  if (ids === undefined || delta === undefined) {
    return undefined
  }
  return {
    id: String(envelope.seq),
    type: "message.part.updated",
    properties: {
      part: {
        id: ids.part,
        sessionID: ids.session,
        messageID: ids.message,
        type: "text",
        text: delta,
        time: { start: envelope.ts_millis },
      },
      delta,
    },
  }
}

function toolResultEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const ids = toolIds(envelope.event)
  if (ids === undefined) {
    return undefined
  }
  const timeMs = numberField(envelope.event, "time_ms") ?? 0
  return {
    id: String(envelope.seq),
    type: "message.part.updated",
    properties: {
      part: {
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
      },
    },
  }
}

function toolErrorEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const ids = toolIds(envelope.event)
  const message = stringField(envelope.event, "message_text")
  if (ids === undefined || message === undefined) {
    return undefined
  }
  return {
    id: String(envelope.seq),
    type: "message.part.updated",
    properties: {
      part: {
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
      },
    },
  }
}

function partIds(event: Readonly<Record<string, unknown>>):
  | { readonly session: string; readonly message: string; readonly part: string }
  | undefined {
  const session = stringField(event, "session")
  const message = stringField(event, "message")
  const part = stringField(event, "part")
  if (session === undefined || message === undefined || part === undefined) {
    return undefined
  }
  return { session, message, part }
}

function toolIds(event: Readonly<Record<string, unknown>>):
  | {
      readonly session: string
      readonly message: string
      readonly part: string
      readonly call: string
    }
  | undefined {
  const base = partIds(event)
  const call = stringField(event, "call")
  if (base === undefined || call === undefined) {
    return undefined
  }
  return { ...base, call }
}

function isEventHook(value: unknown): value is OpenCodeEventHook {
  return typeof value === "function"
}

function optionalString(key: string, value: string | undefined): Record<string, string> {
  return value === undefined ? {} : { [key]: value }
}

function stringField(
  source: Readonly<Record<string, unknown>>,
  key: string,
): string | undefined {
  const value = source[key]
  return typeof value === "string" ? value : undefined
}

function numberField(
  source: Readonly<Record<string, unknown>>,
  key: string,
): number | undefined {
  const value = source[key]
  return typeof value === "number" ? value : undefined
}

function stringifyOutput(output: unknown): string {
  if (typeof output === "string") {
    return output
  }
  if (output === undefined) {
    return ""
  }
  return JSON.stringify(output)
}
