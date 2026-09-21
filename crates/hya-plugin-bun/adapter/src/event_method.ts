import { runEventHooks, type EventEnvelope } from "./event_hooks"
import type { JsonRpcNotification } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"
import { isInt, isRecord, isNonEmptyString, ok, type ValidationResult } from "./validate"

export async function handleEventNotification(
  request: JsonRpcNotification,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateEventParams(request.params)
  if (params.ok) {
    await runEventHooks(context.hooks, params.value)
  }
  return { response: "", shouldExit: false }
}

function validateEventParams(value: unknown): ValidationResult<EventEnvelope> {
  if (!isRecord(value)) {
    return { ok: false, message: "params must be an object" }
  }
  const envelope = value.envelope
  if (!isRecord(envelope)) {
    return { ok: false, message: "params.envelope must be an object" }
  }
  const seq = envelope.seq
  if (!isInt(seq) && typeof seq !== "string") {
    return { ok: false, message: "params.envelope.seq must be an integer or string" }
  }
  if (!isInt(envelope.ts_millis)) {
    return { ok: false, message: "params.envelope.ts_millis must be an integer" }
  }
  const event = envelope.event
  if (!isRecord(event) || !isNonEmptyString(event.type)) {
    return { ok: false, message: "params.envelope.event must be an object with a type" }
  }
  return ok({ seq, ts_millis: envelope.ts_millis, event })
}
