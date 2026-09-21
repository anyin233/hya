import { type ExtensionHooks } from "./loader/init"
import { isRecord } from "./validate"

export type WireResource = { readonly type: string; readonly value?: string }

export type PermissionAskParams = {
  readonly session?: string
  readonly action: string
  readonly resource: WireResource
}

export type PermissionStatus = "allow_once" | "allow_always" | "reject" | "defer"

export type PermissionOutcome =
  | { readonly outcome: "allow_once" }
  | { readonly outcome: "allow_always" }
  | { readonly outcome: "reject"; readonly feedback?: string }
  | { readonly outcome: "defer" }

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

function outcomeFrom(returned: unknown): PermissionOutcome | undefined {
  if (isPermissionStatus(returned)) {
    return { outcome: returned }
  }
  if (isRecord(returned) && isPermissionStatus(returned.outcome)) {
    if (returned.outcome === "reject" && typeof returned.feedback === "string") {
      return { outcome: "reject", feedback: returned.feedback }
    }
    return { outcome: returned.outcome }
  }
  return undefined
}

function isPermissionStatus(value: unknown): value is PermissionStatus {
  return (
    value === "allow_once" ||
    value === "allow_always" ||
    value === "reject" ||
    value === "defer"
  )
}

function isPermissionAskHook(
  value: unknown,
): value is (params: unknown) => unknown | Promise<unknown> {
  return typeof value === "function"
}
