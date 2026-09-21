import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"
import { callRegisteredTool } from "./tool"
import { isNonEmptyString, isRecord } from "./validate"

export async function handleToolCall(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateToolCallParams(request.params)
  if (!params.ok) {
    return {
      response: errorResponse(request.id, ERROR_CODES.INVALID_PARAMS, params.message),
      shouldExit: false,
    }
  }
  const directory = context.env.HYA_DIRECTORY ?? process.cwd()
  const worktree = context.env.HYA_WORKTREE ?? directory
  const reply = await callRegisteredTool(context.tools, params.value, {
    directory,
    worktree,
  })
  return {
    response: okResponse(request.id, reply),
    shouldExit: false,
  }
}

function validateToolCallParams(value: unknown) {
  if (!isRecord(value)) {
    return { ok: false as const, message: "params must be an object" }
  }
  for (const key of ["tool", "session", "call"] as const) {
    if (!isNonEmptyString(value[key])) {
      return { ok: false as const, message: `params.${key} must be a non-empty string` }
    }
  }
  if (!("input" in value)) {
    return { ok: false as const, message: "params.input is required" }
  }
  return {
    ok: true as const,
    value: {
      tool: value.tool as string,
      session: value.session as string,
      call: value.call as string,
      input: value.input,
    },
  }
}
