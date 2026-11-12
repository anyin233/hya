import { isPermissionOutcomeWire } from "./wire.gen"
import { type ExtensionHooks } from "./loader/init"
import { isRecord } from "./validate"

import type { PermissionAskParams, PermissionOutcomeWire as PermissionOutcome } from "./wire.gen"
export type { WireResource, PermissionAskParams, PermissionOutcomeWire as PermissionOutcome } from "./wire.gen"
export type PermissionStatus = PermissionOutcome["outcome"]

/**
 * `permission.ask` handlers receive the hya-shaped params as-is and return
 * `allow_once | allow_always | reject | defer` — either the bare status
 * string or an `{ outcome, feedback? }` record. The returned status maps back
 * verbatim; nothing returned, an unknown status, or a thrown error defers.
 * The first non-defer answer wins.
 */
export async function runPermissionAskHooks(
  hooks: readonly ExtensionHooks[],
  params: PermissionAskParams,
): Promise<PermissionOutcome> {
  for (const hook of hooks) {
    const candidate = hook["permission.ask"]
    if (!isPermissionAskHook(candidate)) {
      continue
    }
    try {
      const returned = await candidate(params)
      const outcome = outcomeFrom(returned)
      if (outcome !== undefined && outcome.outcome !== "defer") {
        return outcome
      }
    } catch {
      continue
    }
  }
  return { outcome: "defer" }
}

export function outcomeFrom(returned: unknown): PermissionOutcome | undefined {
  const candidate = typeof returned === "string" ? { outcome: returned } : returned
  if (!isRecord(candidate)) return undefined
  const wire = candidate.outcome === "reject" && typeof candidate.feedback === "string"
    ? { outcome: candidate.outcome, feedback: candidate.feedback }
    : { outcome: candidate.outcome }
  return isPermissionOutcomeWire(wire) ? wire : undefined
}

function isPermissionAskHook(
  value: unknown,
): value is (params: unknown) => unknown | Promise<unknown> {
  return typeof value === "function"
}
