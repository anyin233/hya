import type { OpenCodeHooks } from "./loader/init"

export type TextCompleteParams = {
  readonly session: string
  readonly message: string
  readonly part: string
  readonly text: string
}

export type TextCompleteOutcome = {
  readonly outcome: "continue"
  readonly text: string
}

type TextCompleteHook = (
  input: Readonly<{
    readonly sessionID: string
    readonly messageID: string
    readonly partID: string
  }>,
  output: { text: string },
) => unknown | Promise<unknown>

export async function runTextCompleteHooks(
  hooks: readonly OpenCodeHooks[],
  params: TextCompleteParams,
): Promise<TextCompleteOutcome> {
  const output = { text: params.text }
  for (const hook of hooks) {
    const candidate = hook["experimental.text.complete"]
    if (!isTextCompleteHook(candidate)) {
      continue
    }
    await candidate(
      {
        sessionID: params.session,
        messageID: params.message,
        partID: params.part,
      },
      output,
    )
  }
  return { outcome: "continue", text: output.text }
}

function isTextCompleteHook(value: unknown): value is TextCompleteHook {
  return typeof value === "function"
}
