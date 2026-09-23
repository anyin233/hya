import type { ExtensionHooks } from "./loader/init"
import { isNonEmptyString, isRecord } from "./validate"

export type ModelFailureClass =
  | "retryable"
  | "unknown_model"
  | "auth"
  | "invalid_request"
  | "other"

export type ModelFallbackParams = {
  readonly session: string
  /** Root of the session's spawn tree; equals `session` for a root. */
  readonly root_session: string
  /** Stable id of the agent bound to `session`. */
  readonly agent?: string
  readonly message: string
  /** Model whose attempt just failed (`provider/model`). */
  readonly model: string
  readonly error: { readonly class: string; readonly message: string }
  /** 1-based count of failed attempts so far in this round. */
  readonly attempt: number
  /** Models already attempted in this round, in order. */
  readonly tried: readonly string[]
}

export type ModelFallbackOutcome =
  | { readonly outcome: "retry"; readonly model: string }
  | { readonly outcome: "give_up" }

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
