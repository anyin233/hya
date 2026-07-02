import {
  compatEventFromEnvelope,
  type EventEnvelope,
  type CompatEvent,
} from "./event_converters"
import type { CompatHooks } from "./loader/init"

type CompatEventHook = (input: { readonly event: CompatEvent }) => unknown | Promise<unknown>

export type { EventEnvelope }

export async function runEventHooks(
  hooks: readonly CompatHooks[],
  envelope: EventEnvelope,
): Promise<void> {
  const event = compatEventFromEnvelope(envelope)
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

function isEventHook(value: unknown): value is CompatEventHook {
  return typeof value === "function"
}
