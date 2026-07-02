import type { CompatHooks } from "./loader/init"

export type HookRegistration = {
  readonly name: string
}

const HOOK_MAPPINGS = [
  ["event", "event"],
  ["command.execute.before", "command.execute.before"],
  ["experimental.text.complete", "experimental.text.complete"],
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
  hooks: readonly CompatHooks[],
): readonly HookRegistration[] {
  const seen = new Set<string>()
  const registrations: HookRegistration[] = []
  for (const hook of hooks) {
    for (const [compatName, hyaName] of HOOK_MAPPINGS) {
      if (seen.has(hyaName) || hook[compatName] === undefined) {
        continue
      }
      seen.add(hyaName)
      registrations.push({ name: hyaName })
    }
  }
  return registrations
}
