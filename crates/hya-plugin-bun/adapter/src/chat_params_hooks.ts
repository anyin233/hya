import { extensionSpec, type ExtensionHooks } from "./loader/init"

import { isWireCompletionRequest, type WireCompletionRequest, type ChatParamsParams as ChatParams, type ChatParamsOutcomeWire as ChatParamsOutcome } from "./wire.gen"
export type { ChatParamsParams as ChatParams, ChatParamsOutcomeWire as ChatParamsOutcome } from "./wire.gen"

/**
 * `chat.params` handlers receive the hya wire params (`session`,
 * `root_session?`, `agent?`, `message`, and the current `request` object) and
 * may return a replacement request record; requests fold across handlers in
 * load order.
 */
export async function runChatParamsHooks(
  hooks: readonly ExtensionHooks[],
  params: ChatParams,
): Promise<ChatParamsOutcome> {
  let current: WireCompletionRequest = params.request
  for (const hook of hooks) {
    const candidate = hook["chat.params"]
    if (!isChatParamsHook(candidate)) {
      continue
    }
    try {
      const returned = await candidate({ ...params, request: current })
      if (isWireCompletionRequest(returned)) {
        current = returned
      } else if (returned !== undefined) {
        console.error(`chat.params: invalid return from extension ${extensionSpec(hook)}`)
      }
    } catch {
      continue
    }
  }
  return { outcome: "continue", request: current }
}

function isChatParamsHook(
  value: unknown,
): value is (params: unknown) => unknown | Promise<unknown> {
  return typeof value === "function"
}
