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

/**
 * Two hook calling conventions are supported, matching the previous adapter
 * generation while allowing the newer single-argument style:
 *
 * - Legacy (declared with two or more parameters): the handler receives
 *   `(input, output)` and mutates `output` in place — `output.args` for
 *   `tool.execute.before`, and the `{title, output, metadata}` record for
 *   `tool.execute.after`. A thrown error vetoes a before-hook.
 * - Single-argument: the handler receives the full wire params and may return
 *   a replacement (`{input}` / `{outcome:"veto", reason}` before, a
 *   `WireToolResult` after); results fold across handlers.
 */
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
      if (candidate.length >= 2) {
        const output = { args: current }
        await candidate(
          { tool: params.tool, sessionID: params.session, callID: params.call },
          output,
        )
        current = output.args
        continue
      }
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
  let legacyOutput: { title: string; output: string; metadata: Record<string, unknown> } | undefined
  for (const hook of hooks) {
    const candidate = hook["tool.execute.after"]
    if (!isHookFunction(candidate)) {
      continue
    }
    try {
      if (candidate.length >= 2) {
        if (legacyOutput === undefined) {
          legacyOutput = compatOutputFromResult(current)
        }
        await candidate(
          {
            tool: params.tool,
            sessionID: params.session,
            callID: params.call,
            args: params.input,
          },
          legacyOutput,
        )
        continue
      }
      const returned = await candidate({ ...params, result: current })
      if (current.status !== "err" && isWireToolResult(returned)) {
        current = returned
      }
    } catch {
      continue
    }
  }
  if (legacyOutput !== undefined) {
    current = wireResultFromOutput(current, legacyOutput)
  }
  return { outcome: "continue", result: current }
}

function isHookFunction(
  value: unknown,
): value is (...args: unknown[]) => unknown | Promise<unknown> {
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

function compatOutputFromResult(result: WireToolResult): {
  title: string
  output: string
  metadata: Record<string, unknown>
} {
  if (result.status === "err") {
    return { title: "", output: result.message, metadata: {} }
  }
  const output = result.output
  if (isRecord(output) && typeof output.output === "string") {
    return {
      title: typeof output.title === "string" ? output.title : "",
      output: output.output,
      metadata: isRecord(output.metadata) ? { ...output.metadata } : {},
    }
  }
  return { title: "", output: stringifyOutput(output), metadata: {} }
}

function wireResultFromOutput(
  original: WireToolResult,
  output: { readonly title: string; readonly output: string; readonly metadata: unknown },
): WireToolResult {
  if (original.status === "err") {
    return original
  }
  const timing = original.time_ms === undefined ? {} : { time_ms: original.time_ms }
  return {
    status: "ok",
    output: {
      title: output.title,
      output: output.output,
      metadata: isRecord(output.metadata) ? output.metadata : {},
    },
    ...timing,
  }
}

function stringifyOutput(output: unknown): string {
  if (typeof output === "string") {
    return output
  }
  if (output === undefined) {
    return ""
  }
  return JSON.stringify(output)
}
