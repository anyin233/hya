import type { OpenCodeHooks } from "./loader/init"

export type CommandExecuteBeforeParams = {
  readonly session: string
  readonly command: string
  readonly arguments: string
  readonly text: string
}

export type CommandBeforeOutcome = {
  readonly outcome: "continue"
  readonly text: string
}

type CommandPartOutput = {
  id?: string
  sessionID?: string
  messageID?: string
  type: "text"
  text: string
}

type CommandBeforeHook = (
  input: Readonly<{
    readonly command: string
    readonly sessionID: string
    readonly arguments: string
  }>,
  output: { parts: CommandPartOutput[] },
) => unknown | Promise<unknown>

export async function runCommandExecuteBeforeHooks(
  hooks: readonly OpenCodeHooks[],
  params: CommandExecuteBeforeParams,
): Promise<CommandBeforeOutcome> {
  const output = commandOutput(params)
  for (const hook of hooks) {
    const candidate = hook["command.execute.before"]
    if (!isCommandBeforeHook(candidate)) {
      continue
    }
    await candidate(
      {
        command: params.command,
        sessionID: params.session,
        arguments: params.arguments,
      },
      output,
    )
  }
  return { outcome: "continue", text: textFromParts(output.parts) }
}

function isCommandBeforeHook(value: unknown): value is CommandBeforeHook {
  return typeof value === "function"
}

function commandOutput(params: CommandExecuteBeforeParams): {
  parts: CommandPartOutput[]
} {
  const messageID = "msg_yaca_command_before"
  return {
    parts: [
      {
        id: "prt_yaca_command_before",
        sessionID: params.session,
        messageID,
        type: "text",
        text: params.text,
      },
    ],
  }
}

function textFromParts(parts: readonly unknown[]): string {
  return parts
    .flatMap((part) =>
      isRecord(part) && part.type === "text" && typeof part.text === "string"
        ? [part.text]
        : [],
    )
    .join("")
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
