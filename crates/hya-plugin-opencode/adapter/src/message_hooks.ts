import type { OpenCodeHooks } from "./loader/init"

export type MessageUserBeforeParams = {
  readonly session: string
  readonly text: string
}

export type MessageUserBeforeOutcome = {
  readonly outcome: "continue"
  readonly text: string
}

type ChatMessageHook = (
  input: Readonly<{
    readonly sessionID: string
    readonly agent?: string
    readonly model?: { readonly providerID: string; readonly modelID: string }
    readonly messageID?: string
    readonly variant?: string
  }>,
  output: {
    message: UserMessageOutput
    parts: MessagePartOutput[]
  },
) => unknown | Promise<unknown>

type UserMessageOutput = {
  id: string
  sessionID: string
  role: "user"
  time: { created: number }
  agent: string
  model: { providerID: string; modelID: string }
}

type MessagePartOutput = {
  id: string
  sessionID: string
  messageID: string
  type: "text"
  text: string
}

export async function runChatMessageHooks(
  hooks: readonly OpenCodeHooks[],
  params: MessageUserBeforeParams,
): Promise<MessageUserBeforeOutcome> {
  const output = openCodeMessageOutput(params)
  for (const hook of hooks) {
    const candidate = hook["chat.message"]
    if (!isChatMessageHook(candidate)) {
      continue
    }
    await candidate({ sessionID: params.session }, output)
  }
  return { outcome: "continue", text: textFromParts(output.parts) }
}

function isChatMessageHook(value: unknown): value is ChatMessageHook {
  return typeof value === "function"
}

function openCodeMessageOutput(params: MessageUserBeforeParams): {
  message: UserMessageOutput
  parts: MessagePartOutput[]
} {
  const messageID = "msg_hya_user_before"
  return {
    message: {
      id: messageID,
      sessionID: params.session,
      role: "user",
      time: { created: 0 },
      agent: "hya",
      model: { providerID: "hya", modelID: "unknown" },
    },
    parts: [
      {
        id: "prt_hya_user_before",
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
