import { isWireToolResult } from "./wire.gen"
import { errorMessage, type ExtensionHooks } from "./loader/init"
import { isRecord } from "./validate"

import type { ToolExecuteBeforeParams, ToolExecuteAfterParams, ToolBeforeOutcomeWire as ToolBeforeOutcome, ToolAfterOutcomeWire as ToolAfterOutcome } from "./wire.gen"
export type { ToolExecuteBeforeParams, ToolExecuteAfterParams, ToolBeforeOutcomeWire as ToolBeforeOutcome, ToolAfterOutcomeWire as ToolAfterOutcome, WireToolResult } from "./wire.gen"

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
