import { type ExtensionHooks } from "./loader/init"
import { outcomeFrom, type PermissionOutcome, type WireResource } from "./permission_hooks"

/** hya's `hook/permission.approve` params (a bundle permission mode's ask). */
export type PermissionApproveParams = {
  readonly session: string
  readonly root_session: string
  readonly agent?: string
  /** Bundle-local id of the active `permission_modes:` entry. */
  readonly mode: string
  readonly action: string
  readonly resource: WireResource
}

/**
 * `permission.approve` handlers receive the hya-shaped params as-is and
 * return `allow_once | allow_always | reject | defer`, either as the bare
 * status string or as an `{ outcome, feedback? }` record. Nothing returned,
 * an unknown status, or a thrown error defers; the first non-defer answer
 * wins. A final `defer` sends the ask to the user.
 */
export async function runPermissionApproveHooks(
  hooks: readonly ExtensionHooks[],
  params: PermissionApproveParams,
): Promise<PermissionOutcome> {
  for (const hook of hooks) {
    const candidate = hook["permission.approve"]
    if (typeof candidate !== "function") {
      continue
    }
    try {
      const outcome = outcomeFrom(await candidate(params))
      if (outcome !== undefined && outcome.outcome !== "defer") {
        return outcome
      }
    } catch {
      continue
    }
  }
  return { outcome: "defer" }
}
