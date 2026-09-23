import type { ExtensionHooks } from "./loader/init"
import { isRecord } from "./validate"

export type ChatParams = {
  readonly session: string
  /** Root of the session's spawn tree; equals `session` for a root. */
  readonly root_session?: string
  /** Stable id of the agent bound to `session`. */
  readonly agent?: string
  readonly message: string
  readonly request: Readonly<Record<string, unknown>>
}

export type ChatParamsOutcome = {
  readonly outcome: "continue"
  readonly request: Readonly<Record<string, unknown>>
}

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
  let current: Readonly<Record<string, unknown>> = params.request
  for (const hook of hooks) {
    const candidate = hook["chat.params"]
    if (!isChatParamsHook(candidate)) {
      continue
    }
    try {
      const returned = await candidate({ ...params, request: current })
      if (isRecord(returned)) {
        current = returned
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
