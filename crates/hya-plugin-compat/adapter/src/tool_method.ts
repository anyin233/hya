import { z } from "zod"

import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"
import { callRegisteredTool } from "./tool"

const ToolCallParamsSchema = z
  .object({
    tool: z.string(),
    session: z.string(),
    call: z.string(),
    input: z.unknown(),
  })
  .strict()

export async function handleToolCall(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = ToolCallParamsSchema.safeParse(request.params)
  if (!params.success) {
    return {
      response: errorResponse(
        request.id,
        ERROR_CODES.INVALID_PARAMS,
        params.error.message,
      ),
      shouldExit: false,
    }
  }
  const directory = context.env.HYA_DIRECTORY ?? process.cwd()
  const worktree = context.env.HYA_WORKTREE ?? directory
  const reply = await callRegisteredTool(context.tools, params.data, {
    directory,
    worktree,
  })
  return {
    response: okResponse(request.id, reply),
    shouldExit: false,
  }
}
