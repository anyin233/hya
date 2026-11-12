import type { ExtensionHooks } from "./loader/init"
import { isNonEmptyString, isRecord } from "./validate"

import type { ModelFallbackParams, ModelFallbackOutcomeWire as ModelFallbackOutcome } from "./wire.gen"
export type { ModelFallbackParams, ModelFallbackOutcomeWire as ModelFallbackOutcome } from "./wire.gen"

/**
 * `model.fallback` handlers receive the hya params as-is and return either
 * `{ outcome: "retry", model }`, a bare non-empty model string (a retry), or
 * `{ outcome: "give_up" }`. Nothing returned, an unknown shape, an empty
 * model, or a thrown error reads as give-up. The first retry wins.
 */
export async function runModelFallbackHooks(
  hooks: readonly ExtensionHooks[],
  params: ModelFallbackParams,
): Promise<ModelFallbackOutcome> {
  for (const hook of hooks) {
    const candidate = hook["model.fallback"]
    if (!isModelFallbackHook(candidate)) {
      continue
    }
    try {
      const model = retryModel(await candidate(params))
      if (model !== undefined) {
        return { outcome: "retry", model }
      }
    } catch {
      continue
    }
  }
  return { outcome: "give_up" }
}

function retryModel(returned: unknown): string | undefined {
  if (isNonEmptyString(returned) && returned.trim().length > 0) {
    return returned
  }
  if (
    isRecord(returned) &&
    returned.outcome === "retry" &&
    isNonEmptyString(returned.model) &&
    returned.model.trim().length > 0
  ) {
    return returned.model
  }
  return undefined
}

function isModelFallbackHook(
  value: unknown,
): value is (params: unknown) => unknown | Promise<unknown> {
  return typeof value === "function"
}
