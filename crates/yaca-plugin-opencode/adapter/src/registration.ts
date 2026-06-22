import type { OpenCodeHooks } from "./loader/init"

export type HookRegistration = {
  readonly name: string
}

const HOOK_MAPPINGS = [
  ["event", "event"],
  ["command.execute.before", "command.execute.before"],
  ["chat.message", "message.user.before"],
  ["chat.params", "chat.params"],
  ["chat.headers", "chat.params"],
  ["experimental.chat.messages.transform", "chat.params"],
  ["experimental.chat.system.transform", "chat.params"],
  ["permission.ask", "permission.ask"],
  ["shell.env", "tool.execute.before"],
  ["tool.execute.before", "tool.execute.before"],
  ["tool.execute.after", "tool.execute.after"],
  ["tool.definition", "chat.params"],
] as const

export function hookRegistrationsFrom(
  hooks: readonly OpenCodeHooks[],
): readonly HookRegistration[] {
  const seen = new Set<string>()
  const registrations: HookRegistration[] = []
  for (const hook of hooks) {
    for (const [openCodeName, yacaName] of HOOK_MAPPINGS) {
      if (seen.has(yacaName) || hook[openCodeName] === undefined) {
        continue
      }
      seen.add(yacaName)
      registrations.push({ name: yacaName })
    }
  }
  return registrations
}
