import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"
import { callRegisteredTool } from "./tool"
import { isToolCallParams } from "./wire.gen"

export async function handleToolCall(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  if (!isToolCallParams(request.params)) {
    return {
      response: errorResponse(request.id, ERROR_CODES.INVALID_PARAMS, "invalid tool/call params"),
      shouldExit: false,
    }
  }
  const reply = await callRegisteredTool(context.tools, request.params)
  return {
    response: okResponse(request.id, reply),
    shouldExit: false,
  }
}
