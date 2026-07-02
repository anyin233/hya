import type { CompatHooks } from "./loader/init"

type ChatMessagesTransformHook = (
  input: Record<string, never>,
  output: ChatMessagesTransformOutput,
) => unknown | Promise<unknown>

type ChatMessagesTransformOutput = {
  messages: CompatHistoryEntry[]
}

type CompatHistoryEntry = {
  info: CompatMessageInfo
  parts: unknown[]
}

type CompatMessageInfo = Readonly<Record<string, unknown>> & {
  readonly id: string
  readonly sessionID: string
  readonly role: "user" | "assistant"
}

type WireMessage = Readonly<Record<string, unknown>> & {
  readonly id: string
  readonly role: "user" | "assistant"
  readonly parts: readonly unknown[]
}

type TextPart = Readonly<Record<string, unknown>> & {
  readonly id: string
  readonly type: "text"
  readonly text: string
}

export async function runChatMessagesTransformHooks(
  hooks: readonly CompatHooks[],
  sessionID: string,
  messages: readonly unknown[],
): Promise<readonly unknown[]> {
  const wireMessages = allWireMessages(messages)
  if (wireMessages === undefined) {
    return messages
  }
  const output: ChatMessagesTransformOutput = {
    messages: wireMessages.map((message) => compatHistoryEntry(sessionID, message)),
  }
  for (const hook of hooks) {
    const candidate = hook["experimental.chat.messages.transform"]
    if (!isChatMessagesTransformHook(candidate)) {
      continue
    }
    try {
      await candidate({}, output)
    } catch (caught) {
      if (caught instanceof Error) {
        continue
      }
      throw caught
    }
  }
  return output.messages.map((entry, index) =>
    hyaMessageFromCompatEntry(entry, wireMessages[index]),
  )
}

function allWireMessages(messages: readonly unknown[]): readonly WireMessage[] | undefined {
  const parsed: WireMessage[] = []
  for (const message of messages) {
    if (!isWireMessage(message)) {
      return undefined
    }
    parsed.push(message)
  }
  return parsed
}

function compatHistoryEntry(sessionID: string, message: WireMessage): CompatHistoryEntry {
  return {
    info: compatMessageInfo(sessionID, message),
    parts: message.parts.map((part) => compatPart(sessionID, message.id, part)),
  }
}

function compatMessageInfo(sessionID: string, message: WireMessage): CompatMessageInfo {
  switch (message.role) {
    case "user":
      return {
        id: message.id,
        sessionID,
        role: "user",
        time: { created: 0 },
        agent: agentName(message),
        model: modelInfo(message),
      }
    case "assistant":
      return {
        id: message.id,
        sessionID,
        role: "assistant",
        time: { created: 0 },
        parentID: "",
        modelID: modelInfo(message).modelID,
        providerID: modelInfo(message).providerID,
        mode: agentName(message),
        path: { cwd: "", root: "" },
        cost: 0,
        tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
      }
  }
}

function compatPart(sessionID: string, messageID: string, part: unknown): unknown {
  if (!isTextPart(part)) {
    return part
  }
  return {
    ...part,
    sessionID,
    messageID,
  }
}

function hyaMessageFromCompatEntry(
  entry: unknown,
  original: WireMessage | undefined,
): unknown {
  if (!isCompatHistoryEntry(entry)) {
    return original ?? entry
  }
  const parts = entry.parts.map((part) => hyaPartFromCompatPart(part, original?.parts ?? []))
  switch (entry.info.role) {
    case "user":
      if (original === undefined) {
        return { role: "user", id: entry.info.id, parts }
      }
      return { ...original, role: "user", id: entry.info.id, parts }
    case "assistant":
      if (original === undefined) {
        return {
          role: "assistant",
          id: entry.info.id,
          agent: agentName(entry.info),
          model: modelRef(entry.info),
          parts,
        }
      }
      return {
        ...original,
        role: "assistant",
        id: entry.info.id,
        parts,
      }
  }
}

function hyaPartFromCompatPart(part: unknown, originalParts: readonly unknown[]): unknown {
  if (!isTextPart(part)) {
    return part
  }
  const original = originalParts.find((candidate) => isTextPart(candidate) && candidate.id === part.id)
  if (original === undefined) {
    return { type: "text", id: part.id, text: part.text }
  }
  return { ...original, type: "text", id: part.id, text: part.text }
}

function modelInfo(message: Readonly<Record<string, unknown>>): {
  readonly providerID: string
  readonly modelID: string
} {
  const model = typeof message["model"] === "string" ? message["model"] : "unknown"
  const slash = model.indexOf("/")
  if (slash < 0) {
    return { providerID: "hya", modelID: model }
  }
  return { providerID: model.slice(0, slash), modelID: model.slice(slash + 1) }
}

function modelRef(message: Readonly<Record<string, unknown>>): string {
  const providerID = typeof message["providerID"] === "string" ? message["providerID"] : "hya"
  const modelID = typeof message["modelID"] === "string" ? message["modelID"] : "unknown"
  return `${providerID}/${modelID}`
}

function agentName(message: Readonly<Record<string, unknown>>): string {
  if (typeof message["agent"] === "string") {
    return message["agent"]
  }
  if (typeof message["mode"] === "string") {
    return message["mode"]
  }
  return "hya"
}

function isChatMessagesTransformHook(value: unknown): value is ChatMessagesTransformHook {
  return typeof value === "function"
}

function isWireMessage(value: unknown): value is WireMessage {
  if (!isRecord(value)) {
    return false
  }
  return (
    typeof value["id"] === "string" &&
    (value["role"] === "user" || value["role"] === "assistant") &&
    Array.isArray(value["parts"])
  )
}

function isCompatHistoryEntry(value: unknown): value is CompatHistoryEntry {
  if (!isRecord(value) || !Array.isArray(value["parts"])) {
    return false
  }
  return isCompatMessageInfo(value["info"])
}

function isCompatMessageInfo(value: unknown): value is CompatMessageInfo {
  if (!isRecord(value)) {
    return false
  }
  return (
    typeof value["id"] === "string" &&
    typeof value["sessionID"] === "string" &&
    (value["role"] === "user" || value["role"] === "assistant")
  )
}

function isTextPart(value: unknown): value is TextPart {
  if (!isRecord(value)) {
    return false
  }
  return (
    value["type"] === "text" &&
    typeof value["id"] === "string" &&
    typeof value["text"] === "string"
  )
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
