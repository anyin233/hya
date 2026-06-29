import { z } from "zod"

import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"
import { runTextCompleteHooks } from "./text_complete_hooks"

const TextCompleteParamsSchema = z
  .object({
    session: z.string(),
    message: z.string(),
    part: z.string(),
    text: z.string(),
  })
  .strict()

export async function handleTextComplete(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = TextCompleteParamsSchema.safeParse(request.params)
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
  const outcome = await runTextCompleteHooks(context.hooks, params.data)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}
