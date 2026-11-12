import type { ExtensionHooks } from "./loader/init"

export type TextHookName =
  | "message.user.before"
  | "command.execute.before"
  | "experimental.text.complete"

import type { HookParams, MessageUserBeforeOutcomeWire as TextOutcome } from "./wire.gen"
export type { MessageUserBeforeOutcomeWire as TextOutcome } from "./wire.gen"

/**
 * Enrichment hooks that rewrite a text payload (`message.user.before`,
 * `command.execute.before`, `experimental.text.complete`). Each handler
 * receives the hya wire params and may return the replacement text as a
 * string; outputs fold across handlers in load order.
 */
export async function runTextHooks(
  hooks: readonly ExtensionHooks[],
  name: TextHookName,
  params: HookParams[TextHookName],
): Promise<TextOutcome> {
  let current = params.text
  for (const hook of hooks) {
    const candidate = hook[name]
    if (!isTextHook(candidate)) {
      continue
    }
    try {
      const returned = await candidate({ ...params, text: current })
      if (typeof returned === "string") {
        current = returned
      }
    } catch {
      continue
    }
  }
  return { outcome: "continue", text: current }
}

function isTextHook(
  value: unknown,
): value is (params: unknown) => unknown | Promise<unknown> {
  return typeof value === "function"
}
