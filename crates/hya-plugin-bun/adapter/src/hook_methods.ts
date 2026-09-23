import { runChatParamsHooks, type ChatParams } from "./chat_params_hooks"
import {
  runToolExecuteAfterHooks,
  runToolExecuteBeforeHooks,
  type ToolAfterOutcome,
  type ToolBeforeOutcome,
  type WireToolResult,
} from "./hooks"
import { runPermissionAskHooks, type PermissionAskParams, type PermissionOutcome } from "./permission_hooks"
import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import { runTextHooks, type TextOutcome } from "./text_hooks"
import type { HandledRequest, RequestContext } from "./runtime_types"
import {
  isInt,
  isNonEmptyString,
  isRecord,
  isRecordMutable,
  ok,
  recordWithStrings,
  type ValidationResult,
} from "./validate"

export async function handleMessageUserBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  return handleTextHook(request, context, "message.user.before", ["session", "text"])
}

export async function handleCommandExecuteBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  return handleTextHook(request, context, "command.execute.before", [
    "session",
    "command",
    "arguments",
    "text",
  ])
}

export async function handleTextComplete(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  return handleTextHook(request, context, "experimental.text.complete", [
    "session",
    "message",
    "part",
    "text",
  ])
}

async function handleTextHook(
  request: JsonRpcRequest,
  context: RequestContext,
  hookName: Parameters<typeof runTextHooks>[1],
  required: readonly string[],
): Promise<HandledRequest> {
  const params = recordWithStrings(request.params, required)
  if (!params.ok) {
    return invalidParams(request.id, params.message)
  }
  const text = params.value.text
  if (typeof text !== "string") {
    return invalidParams(request.id, "params.text is invalid")
  }
  const outcome: TextOutcome = await runTextHooks(
    context.hooks,
    hookName,
    params.value as Readonly<Record<string, unknown>> & { readonly text: string },
  )
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

export async function handleChatParams(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateChatParams(request.params)
  if (!params.ok) {
    return invalidParams(request.id, params.message)
  }
  const outcome = await runChatParamsHooks(context.hooks, params.value)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

export async function handlePermissionAsk(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validatePermissionParams(request.params)
  if (!params.ok) {
    return invalidParams(request.id, params.message)
  }
  const outcome: PermissionOutcome = await runPermissionAskHooks(context.hooks, params.value)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

export async function handleToolExecuteBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateToolExecuteBeforeParams(request.params)
  if (!params.ok) {
    return invalidParams(request.id, params.message)
  }
  const outcome: ToolBeforeOutcome = await runToolExecuteBeforeHooks(
    context.hooks,
    params.value,
  )
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

export async function handleToolExecuteAfter(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateToolExecuteAfterParams(request.params)
  if (!params.ok) {
    return invalidParams(request.id, params.message)
  }
  const outcome: ToolAfterOutcome = await runToolExecuteAfterHooks(context.hooks, params.value)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

function invalidParams(id: number, message: string): HandledRequest {
  return {
    response: errorResponse(id, ERROR_CODES.INVALID_PARAMS, message),
    shouldExit: false,
  }
}

function validateChatParams(value: unknown): ValidationResult<ChatParams> {
  const params = recordWithStrings(value, ["session", "message"], ["root_session", "agent"])
  if (!params.ok) {
    return params
  }
  if (!isRecord(params.value.request)) {
    return { ok: false, message: "params.request must be an object" }
  }
  return ok({
    session: params.value.session as string,
    ...optionalStrings(params.value, ["root_session", "agent"]),
    message: params.value.message as string,
    request: params.value.request,
  })
}

/** Copy the present (already validated) optional string fields. */
function optionalStrings(
  record: Readonly<Record<string, unknown>>,
  keys: readonly string[],
): Record<string, string> {
  const present: Record<string, string> = {}
  for (const key of keys) {
    const value = record[key]
    if (typeof value === "string") {
      present[key] = value
    }
  }
  return present
}

function validatePermissionParams(
  value: unknown,
): ValidationResult<PermissionAskParams> {
  const params = recordWithStrings(value, ["action"], ["session"])
  if (!params.ok) {
    return params
  }
  const resource = params.value.resource
  if (!isRecord(resource) || !isNonEmptyString(resource.type)) {
    return { ok: false, message: "params.resource must be an object with a type" }
  }
  if (resource.value !== undefined && typeof resource.value !== "string") {
    return { ok: false, message: "params.resource.value must be a string" }
  }
  const wireResource: PermissionAskParams["resource"] =
    resource.value === undefined
      ? { type: resource.type }
      : { type: resource.type, value: resource.value }
  const outcome: PermissionAskParams = {
    action: params.value.action as string,
    resource: wireResource,
  }
  if (params.value.session !== undefined) {
    return ok({ ...outcome, session: params.value.session as string })
  }
  return ok(outcome)
}

function validateToolExecuteBeforeParams(
  value: unknown,
): ValidationResult<Parameters<typeof runToolExecuteBeforeHooks>[1]> {
  const params = recordWithStrings(value, ["session", "message", "call", "tool"])
  if (!params.ok) {
    return params
  }
  if (!("input" in params.value)) {
    return { ok: false, message: "params.input is required" }
  }
  return ok({
    session: params.value.session as string,
    message: params.value.message as string,
    call: params.value.call as string,
    tool: params.value.tool as string,
    input: params.value.input,
  })
}

function validateToolExecuteAfterParams(
  value: unknown,
): ValidationResult<Parameters<typeof runToolExecuteAfterHooks>[1]> {
  const params = recordWithStrings(value, ["session", "message", "call", "tool"])
  if (!params.ok) {
    return params
  }
  if (!("input" in params.value)) {
    return { ok: false, message: "params.input is required" }
  }
  const result = validateWireToolResult(params.value.result)
  if (!result.ok) {
    return result
  }
  return ok({
    session: params.value.session as string,
    message: params.value.message as string,
    call: params.value.call as string,
    tool: params.value.tool as string,
    input: params.value.input,
    result: result.value,
  })
}

function validateWireToolResult(value: unknown): ValidationResult<WireToolResult> {
  if (!isRecordMutable(value)) {
    return { ok: false, message: "params.result must be an object" }
  }
  if (value.status === "err") {
    if (!isNonEmptyString(value.message)) {
      return { ok: false, message: "params.result.message must be a non-empty string" }
    }
    return ok({ status: "err", message: value.message })
  }
  if (value.status !== "ok") {
    return { ok: false, message: "params.result.status must be \"ok\" or \"err\"" }
  }
  if (!("output" in value)) {
    return { ok: false, message: "params.result.output is required" }
  }
  if (value.time_ms !== undefined && !isInt(value.time_ms)) {
    return { ok: false, message: "params.result.time_ms must be an integer" }
  }
  const result: WireToolResult = { status: "ok", output: value.output }
  if (value.time_ms !== undefined) {
    return ok({ ...result, time_ms: value.time_ms })
  }
  return ok(result)
}
