import type { OpenCodeHooks } from "./loader/init"

export type ToolExecuteBeforeParams = {
  readonly session: string
  readonly message: string
  readonly call: string
  readonly tool: string
  readonly input: unknown
}

export type ToolExecuteBeforeContext = {
  readonly cwd: string
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

type ToolBeforeHook = (
  input: Readonly<{
    readonly tool: string
    readonly sessionID: string
    readonly callID: string
  }>,
  output: { args: unknown },
) => unknown | Promise<unknown>

type ToolAfterHook = (
  input: Readonly<{
    readonly tool: string
    readonly sessionID: string
    readonly callID: string
    readonly args: unknown
  }>,
  output: {
    title: string
    output: string
    metadata: Record<string, unknown>
  },
) => unknown | Promise<unknown>

type ShellEnvHook = (
  input: Readonly<{
    readonly cwd: string
    readonly sessionID?: string
    readonly callID?: string
  }>,
  output: { env: Record<string, string> },
) => unknown | Promise<unknown>

export async function runToolExecuteBeforeHooks(
  hooks: readonly OpenCodeHooks[],
  params: ToolExecuteBeforeParams,
  context: ToolExecuteBeforeContext,
): Promise<ToolBeforeOutcome> {
  let current = params.input
  for (const hook of hooks) {
    const candidate = hook["tool.execute.before"]
    if (!isToolBeforeHook(candidate)) {
      continue
    }
    const output = { args: current }
    try {
      await candidate(
        {
          tool: params.tool,
          sessionID: params.session,
          callID: params.call,
        },
        output,
      )
      current = output.args
    } catch (error) {
      return { outcome: "veto", reason: errorMessage(error) }
    }
  }
  current = await inputWithShellEnv(hooks, params, context, current)
  return { outcome: "continue", input: current }
}

export async function runToolExecuteAfterHooks(
  hooks: readonly OpenCodeHooks[],
  params: ToolExecuteAfterParams,
): Promise<ToolAfterOutcome> {
  const output = openCodeOutputFromResult(params.result)
  for (const hook of hooks) {
    const candidate = hook["tool.execute.after"]
    if (!isToolAfterHook(candidate)) {
      continue
    }
    await candidate(
      {
        tool: params.tool,
        sessionID: params.session,
        callID: params.call,
        args: params.input,
      },
      output,
    )
  }
  return { outcome: "continue", result: wireResultFromOutput(params.result, output) }
}

function isToolBeforeHook(value: unknown): value is ToolBeforeHook {
  return typeof value === "function"
}

function isToolAfterHook(value: unknown): value is ToolAfterHook {
  return typeof value === "function"
}

function isShellEnvHook(value: unknown): value is ShellEnvHook {
  return typeof value === "function"
}

async function inputWithShellEnv(
  hooks: readonly OpenCodeHooks[],
  params: ToolExecuteBeforeParams,
  context: ToolExecuteBeforeContext,
  input: unknown,
): Promise<unknown> {
  if (params.tool !== "shell") {
    return input
  }
  const output: { env: Record<string, string> } = { env: {} }
  for (const hook of hooks) {
    const candidate = hook["shell.env"]
    if (!isShellEnvHook(candidate)) {
      continue
    }
    try {
      await candidate(
        {
          cwd: context.cwd,
          sessionID: params.session,
          callID: params.call,
        },
        output,
      )
    } catch (caught) {
      if (caught instanceof Error) {
        continue
      }
      throw caught
    }
  }
  return mergeShellEnv(input, output.env)
}

function mergeShellEnv(input: unknown, env: Record<string, string>): unknown {
  if (Object.keys(env).length === 0 || !isRecord(input)) {
    return input
  }
  const existing = isStringRecord(input.env) ? input.env : {}
  return { ...input, env: { ...existing, ...env } }
}

function openCodeOutputFromResult(result: WireToolResult): {
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

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message
  }
  return String(error)
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isStringRecord(value: unknown): value is Readonly<Record<string, string>> {
  if (!isRecord(value)) {
    return false
  }
  return Object.values(value).every((item) => typeof item === "string")
}
