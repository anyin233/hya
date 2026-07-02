import path from "node:path"
import { z } from "zod"

import { runChatParamsHooks } from "./chat_params_hooks"
import { runCommandExecuteBeforeHooks } from "./command_hooks"
import { runToolExecuteAfterHooks, runToolExecuteBeforeHooks } from "./hooks"
import { runChatMessageHooks } from "./message_hooks"
import { runPermissionAskHooks } from "./permission_hooks"
import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"

const MessageUserBeforeParamsSchema = z
  .object({
    session: z.string(),
    text: z.string(),
  })
  .strict()

const WireResourceSchema = z.union([
  z.object({ type: z.literal("any") }).strict(),
  z.object({ type: z.string(), value: z.string() }).strict(),
])

const PermissionAskParamsSchema = z
  .object({
    session: z.string().optional(),
    action: z.string(),
    resource: WireResourceSchema,
  })
  .strict()

const WireCompletionRequestSchema = z
  .object({
    model: z.string(),
    system: z.string().optional(),
    messages: z.array(z.unknown()),
    tools: z.array(z.unknown()),
    temperature: z.number().optional(),
    max_output_tokens: z.number().int().nonnegative().optional(),
    reasoning: z.string().optional(),
    headers: z.record(z.string(), z.string()).optional(),
  })
  .strict()

const ChatParamsParamsSchema = z
  .object({
    session: z.string(),
    message: z.string(),
    request: WireCompletionRequestSchema,
  })
  .strict()

const CommandExecuteBeforeParamsSchema = z
  .object({
    session: z.string(),
    command: z.string(),
    arguments: z.string(),
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

export async function handleChatParams(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = ChatParamsParamsSchema.safeParse(request.params)
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
  const outcome = await runChatParamsHooks(context.hooks, params.data)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

export async function handleCommandExecuteBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = CommandExecuteBeforeParamsSchema.safeParse(request.params)
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
  const outcome = await runCommandExecuteBeforeHooks(context.hooks, params.data)
  return {
    response: okResponse(request.id, outcome),
    shouldExit: false,
  }
}

export async function handlePermissionAsk(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = PermissionAskParamsSchema.safeParse(request.params)
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
  const outcome = await runPermissionAskHooks(context.hooks, params.data)
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
  const outcome = await runToolExecuteBeforeHooks(context.hooks, params.data, {
    cwd: toolExecutionCwd(params.data, context),
  })
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

function toolExecutionCwd(
  params: z.infer<typeof ToolExecuteBeforeParamsSchema>,
  context: RequestContext,
): string {
  const base = context.env.HYA_WORKTREE ?? context.env.HYA_DIRECTORY ?? process.cwd()
  if (!isRecord(params.input) || typeof params.input["workdir"] !== "string") {
    return base
  }
  const workdir = params.input["workdir"]
  return path.isAbsolute(workdir) ? workdir : path.join(base, workdir)
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
