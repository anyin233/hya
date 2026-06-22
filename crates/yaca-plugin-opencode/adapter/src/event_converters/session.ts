import { optionalString, stringField } from "./fields"
import type { EventEnvelope, OpenCodeEvent } from "./types"

export function sessionCreatedEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const session = stringField(envelope.event, "session")
  const workdir = stringField(envelope.event, "workdir")
  if (session === undefined || workdir === undefined) {
    return undefined
  }
  return sessionEvent(envelope, session, workdir, "")
}

export function sessionTitledEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const session = stringField(envelope.event, "session")
  const title = stringField(envelope.event, "title")
  if (session === undefined || title === undefined) {
    return undefined
  }
  return sessionEvent(envelope, session, "", title, "session.updated")
}

export function commandExecutedEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const session = stringField(envelope.event, "session")
  const command = stringField(envelope.event, "command")
  const argumentsValue = stringField(envelope.event, "arguments")
  const message = stringField(envelope.event, "message")
  if (
    session === undefined ||
    command === undefined ||
    argumentsValue === undefined ||
    message === undefined
  ) {
    return undefined
  }
  return {
    id: String(envelope.seq),
    type: "command.executed",
    properties: {
      name: command,
      sessionID: session,
      arguments: argumentsValue,
      messageID: message,
    },
  }
}

export function messageStartedEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const session = stringField(envelope.event, "session")
  const message = stringField(envelope.event, "message")
  const role = stringField(envelope.event, "role")
  if (session === undefined || message === undefined) {
    return undefined
  }
  if (role === "user") {
    return userMessageEvent(envelope, session, message)
  }
  if (role === "assistant") {
    return assistantMessageEvent(envelope, session, message)
  }
  return undefined
}

export function errorEvent(envelope: EventEnvelope): OpenCodeEvent | undefined {
  const message = stringField(envelope.event, "message")
  if (message === undefined) {
    return undefined
  }
  const code = stringField(envelope.event, "code")
  return {
    id: String(envelope.seq),
    type: "session.error",
    properties: {
      ...optionalString("sessionID", stringField(envelope.event, "session")),
      error: {
        name: "UnknownError",
        data: { message: code === undefined ? message : `${code}: ${message}` },
      },
    },
  }
}

function sessionEvent(
  envelope: EventEnvelope,
  session: string,
  directory: string,
  title: string,
  type = "session.created",
): OpenCodeEvent {
  return {
    id: String(envelope.seq),
    type,
    properties: {
      info: {
        id: session,
        projectID: session,
        directory,
        ...optionalString("parentID", stringField(envelope.event, "parent")),
        title,
        version: "0",
        time: { created: envelope.ts_millis, updated: envelope.ts_millis },
      },
    },
  }
}

function userMessageEvent(
  envelope: EventEnvelope,
  session: string,
  message: string,
): OpenCodeEvent {
  return {
    id: String(envelope.seq),
    type: "message.updated",
    properties: {
      info: {
        id: message,
        sessionID: session,
        role: "user",
        time: { created: envelope.ts_millis },
        agent: "yaca",
        model: { providerID: "yaca", modelID: "unknown" },
      },
    },
  }
}

function assistantMessageEvent(
  envelope: EventEnvelope,
  session: string,
  message: string,
): OpenCodeEvent {
  return {
    id: String(envelope.seq),
    type: "message.updated",
    properties: {
      info: {
        id: message,
        sessionID: session,
        role: "assistant",
        time: { created: envelope.ts_millis },
        parentID: "",
        modelID: "unknown",
        providerID: "yaca",
        mode: "build",
        path: { cwd: "", root: "" },
        cost: 0,
        tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
      },
    },
  }
}
