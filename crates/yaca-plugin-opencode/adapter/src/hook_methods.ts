import { z } from "zod"

import { runToolExecuteAfterHooks, runToolExecuteBeforeHooks } from "./hooks"
import { runChatMessageHooks } from "./message_hooks"
import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"

const MessageUserBeforeParamsSchema = z
  .object({
    session: z.string(),
    text: z.string(),
  })
  .strict()

const ToolExecuteBeforeParamsSchema = z
  .object({
    session: z.string(),
    message: z.string(),
    call: z.string(),
    tool: z.string(),
    input: z.unknown(),
  })
  .strict()

const WireToolResultSchema = z.union([
  z
    .object({
      status: z.literal("ok"),
      output: z.unknown(),
      time_ms: z.number().int().nonnegative().optional(),
    })
    .strict(),
  z
    .object({
      status: z.literal("err"),
      message: z.string(),
    })
    .strict(),
])

const ToolExecuteAfterParamsSchema = z
  .object({
    session: z.string(),
    message: z.string(),
    call: z.string(),
    tool: z.string(),
    input: z.unknown(),
    result: WireToolResultSchema,
  })
  .strict()

export async function handleMessageUserBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = MessageUserBeforeParamsSchema.safeParse(request.params)
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
  const outcome = await runChatMessageHooks(context.hooks, params.data)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

export async function handleToolExecuteBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = ToolExecuteBeforeParamsSchema.safeParse(request.params)
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
  const outcome = await runToolExecuteBeforeHooks(context.hooks, params.data)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

export async function handleToolExecuteAfter(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = ToolExecuteAfterParamsSchema.safeParse(request.params)
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
  const outcome = await runToolExecuteAfterHooks(context.hooks, params.data)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}
