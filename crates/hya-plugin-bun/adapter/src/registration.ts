import type { HookRegistration } from "./contributions"
import type { ExtensionHooks } from "./loader/init"

export type { HookRegistration } from "./contributions"

/**
 * The hya wire hook names the adapter dispatches. Extensions register
 * handlers under these exact names on their server result object.
 */
export const HOOK_NAMES = [
  "event",
  "command.execute.before",
  "experimental.text.complete",
  "message.user.before",
  "chat.params",
  "tool.execute.before",
  "tool.execute.after",
  "permission.ask",
] as const

export type HookName = (typeof HOOK_NAMES)[number]

export function hookRegistrationsFrom(
  hooks: readonly ExtensionHooks[],
): readonly HookRegistration[] {
  const seen = new Set<string>()
  const registrations: HookRegistration[] = []
  for (const hook of hooks) {
    for (const name of HOOK_NAMES) {
      if (seen.has(name) || hook[name] === undefined) {
        continue
      }
      seen.add(name)
      registrations.push({ name })
    }
  }
  return registrations
}
