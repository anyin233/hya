import type { OpenCodeHooks } from "./loader/init"

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

type ToolBeforeHook = (
  input: Readonly<{
    readonly tool: string
    readonly sessionID: string
    readonly callID: string
  }>,
  output: { args: unknown },
) => unknown | Promise<unknown>

export async function runToolExecuteBeforeHooks(
  hooks: readonly OpenCodeHooks[],
  params: ToolExecuteBeforeParams,
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
  return { outcome: "continue", input: current }
}

function isToolBeforeHook(value: unknown): value is ToolBeforeHook {
  return typeof value === "function"
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message
  }
  return String(error)
}
