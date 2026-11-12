import { errorMessage, type ExtensionHooks } from "./loader/init"
import { isRecord } from "./validate"

export type ToolExecuteBeforeParams = {
  readonly session: string
  readonly message: string
  readonly call: string
  readonly tool: string
  readonly input: unknown
}

export type ToolBeforeOutcome =
  | { readonly outcome: "continue"; readonly input: unknown }
  | { readonly outcome: "veto"; readonly reason: string }

export type WireToolResult =
  | { readonly status: "ok"; readonly output: unknown; readonly time_ms?: number }
  | { readonly status: "err"; readonly message: string }

export type ToolExecuteAfterParams = {
  readonly session: string
  readonly message: string
  readonly call: string
  readonly tool: string
  readonly input: unknown
  readonly result: WireToolResult
}

export type ToolAfterOutcome = {
  readonly outcome: "continue"
  readonly result: WireToolResult
}

/** Single-argument wire hooks return replacements that fold across handlers. */
export async function runToolExecuteBeforeHooks(
  hooks: readonly ExtensionHooks[],
  params: ToolExecuteBeforeParams,
): Promise<ToolBeforeOutcome> {
  let current = params.input
  for (const hook of hooks) {
    const candidate = hook["tool.execute.before"]
    if (!isHookFunction(candidate)) {
      continue
    }
    try {
      const returned = await candidate({ ...params, input: current })
      if (isRecord(returned) && returned.outcome === "veto") {
        const reason = typeof returned.reason === "string" ? returned.reason : "vetoed"
        return { outcome: "veto", reason }
      }
      if (isRecord(returned) && "input" in returned) {
        current = returned.input
      }
    } catch (error) {
      return { outcome: "veto", reason: errorMessage(error) }
    }
  }
  return { outcome: "continue", input: current }
}

export async function runToolExecuteAfterHooks(
  hooks: readonly ExtensionHooks[],
  params: ToolExecuteAfterParams,
): Promise<ToolAfterOutcome> {
  let current = params.result
  for (const hook of hooks) {
    const candidate = hook["tool.execute.after"]
    if (!isHookFunction(candidate)) {
      continue
    }
    try {
      const returned = await candidate({ ...params, result: current })
      if (current.status !== "err" && isWireToolResult(returned)) {
        current = returned
      }
    } catch {
      continue
    }
  }
  return { outcome: "continue", result: current }
}

function isHookFunction(
  value: unknown,
): value is (params: unknown) => unknown | Promise<unknown> {
  return typeof value === "function"
}

function isWireToolResult(value: unknown): value is WireToolResult {
  if (!isRecord(value)) {
    return false
  }
  if (value.status === "err") {
    return typeof value.message === "string"
  }
  if (value.status !== "ok") {
    return false
  }
  return value.time_ms === undefined || typeof value.time_ms === "number"
}
