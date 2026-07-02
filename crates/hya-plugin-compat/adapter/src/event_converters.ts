import {
  commandExecutedEvent,
  messageFinishedEvent,
  messageStartedEvent,
  sessionCreatedEvent,
  sessionTitledEvent,
} from "./event_converters/session"
import {
  reasoningPartEvent,
  textPartEvent,
  toolCallRequestedEvent,
  toolErrorEvent,
  toolInputDeltaEvent,
  toolInputStartEvent,
  toolResultEvent,
} from "./event_converters/parts"
import { errorEvent } from "./event_converters/session"
import type { EventEnvelope, CompatEvent } from "./event_converters/types"

export type { EventEnvelope, CompatEvent }

export function compatEventFromEnvelope(
  envelope: EventEnvelope,
): CompatEvent | undefined {
  const event = envelope.event
  switch (event.type) {
    case "session_created":
      return sessionCreatedEvent(envelope)
    case "session_titled":
      return sessionTitledEvent(envelope)
    case "command_executed":
      return commandExecutedEvent(envelope)
    case "message_started":
      return messageStartedEvent(envelope)
    case "message_finished":
      return messageFinishedEvent(envelope)
    case "text_start":
      return textPartEvent(envelope, "")
    case "text_delta":
      return textPartEvent(envelope, stringEventField(event, "delta"), stringEventField(event, "delta"))
    case "text_replace":
      return textPartEvent(envelope, stringEventField(event, "text"))
    case "text_end":
      return textPartEvent(envelope, "", undefined, true)
    case "reasoning_start":
      return reasoningPartEvent(envelope, "")
    case "reasoning_delta":
      return reasoningPartEvent(envelope, stringEventField(event, "delta"), stringEventField(event, "delta"))
    case "reasoning_end":
      return reasoningPartEvent(envelope, "", undefined, true)
    case "tool_input_start":
      return toolInputStartEvent(envelope)
    case "tool_input_delta":
      return toolInputDeltaEvent(envelope)
    case "tool_call_requested":
      return toolCallRequestedEvent(envelope)
    case "error":
      return errorEvent(envelope)
    case "tool_result":
      return toolResultEvent(envelope)
    case "tool_error":
      return toolErrorEvent(envelope)
    default:
      return undefined
  }
}

function stringEventField(
  source: Readonly<Record<string, unknown>>,
  key: string,
): string | undefined {
  const value = source[key]
  return typeof value === "string" ? value : undefined
}
