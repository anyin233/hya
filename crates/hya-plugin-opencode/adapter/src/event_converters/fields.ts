import type { EventEnvelope, OpenCodeEvent } from "./types"

export function partEvent(
  envelope: EventEnvelope,
  part: Readonly<Record<string, unknown>>,
  delta?: string,
): OpenCodeEvent {
  return {
    id: String(envelope.seq),
    type: "message.part.updated",
    properties: {
      part,
      ...(delta === undefined ? {} : { delta }),
    },
  }
}

export function partIds(event: Readonly<Record<string, unknown>>):
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

export function toolIds(event: Readonly<Record<string, unknown>>):
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

export function optionalString(key: string, value: string | undefined): Record<string, string> {
  return value === undefined ? {} : { [key]: value }
}

export function stringField(
  source: Readonly<Record<string, unknown>>,
  key: string,
): string | undefined {
  const value = source[key]
  return typeof value === "string" ? value : undefined
}

export function numberField(
  source: Readonly<Record<string, unknown>>,
  key: string,
): number | undefined {
  const value = source[key]
  return typeof value === "number" ? value : undefined
}

export function stringifyOutput(output: unknown): string {
  if (typeof output === "string") {
    return output
  }
  if (output === undefined) {
    return ""
  }
  return JSON.stringify(output)
}

export function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
