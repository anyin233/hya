import {
  openCodeEventFromEnvelope,
  type EventEnvelope,
  type OpenCodeEvent,
} from "./event_converters"
import type { OpenCodeHooks } from "./loader/init"

type OpenCodeEventHook = (input: { readonly event: OpenCodeEvent }) => unknown | Promise<unknown>

export type { EventEnvelope }

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

function isEventHook(value: unknown): value is OpenCodeEventHook {
  return typeof value === "function"
}
