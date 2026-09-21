import type { ExtensionHooks } from "./loader/init"

export type EventEnvelope = {
  readonly seq: number | string
  readonly ts_millis: number
  readonly event: Readonly<Record<string, unknown>>
}

/**
 * `event` handlers receive the raw hya envelope `{ seq, ts_millis, event }`
 * as-is. Delivery is best-effort: a throwing handler is skipped.
 */
export async function runEventHooks(
  hooks: readonly ExtensionHooks[],
  envelope: EventEnvelope,
): Promise<void> {
  for (const hook of hooks) {
    const candidate = hook.event
    if (!isEventHook(candidate)) {
      continue
    }
    try {
      await candidate(envelope)
    } catch {
      continue
    }
  }
}

function isEventHook(
  value: unknown,
): value is (envelope: EventEnvelope) => unknown | Promise<unknown> {
  return typeof value === "function"
}
