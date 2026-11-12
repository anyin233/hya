import { HOOK_METHODS } from "./wire.gen"
import { runStdio } from "./stdio"
export { readLines } from "./stdio"
import { handleEventNotification } from "./event_method"
import { handleInitialize, PROTOCOL_VERSION } from "./initialize"
import {
  ERROR_CODES,
  errorResponse,
  okResponse,
  type JsonRpcMessage,
  type JsonRpcRequest,
} from "./protocol"
import {
  createRequestContext,
  type HandledRequest,
  type RequestContext,
  type RuntimeOptions,
} from "./runtime_types"
import { errorMessage } from "./loader/init"
import {
  handleChatParams,
  handleCommandExecuteBefore,
  handleMessageUserBefore,
  handleModelFallback,
  handlePermissionApprove,
  handlePermissionAsk,
  handleTextComplete,
  handleToolExecuteAfter,
  handleToolExecuteBefore,
} from "./hook_methods"
import { handleToolCall } from "./tool_method"

export { PROTOCOL_VERSION }
export type { RuntimeOptions }

const METHOD_INITIALIZE = "initialize"
const METHOD_SHUTDOWN = "shutdown"
const METHOD_EVENT = HOOK_METHODS.Event
const METHOD_TOOL_CALL = "tool/call"
const METHOD_MESSAGE_USER_BEFORE = HOOK_METHODS.MessageUserBefore
const METHOD_CHAT_PARAMS = HOOK_METHODS.ChatParams
const METHOD_COMMAND_EXECUTE_BEFORE = HOOK_METHODS.CommandExecuteBefore
const METHOD_TEXT_COMPLETE = HOOK_METHODS.TextComplete
const METHOD_PERMISSION_ASK = HOOK_METHODS.PermissionAsk
const METHOD_PERMISSION_APPROVE = HOOK_METHODS.PermissionApprove
const METHOD_MODEL_FALLBACK = HOOK_METHODS.ModelFallback
const METHOD_TOOL_EXECUTE_BEFORE = HOOK_METHODS.ToolExecuteBefore
const METHOD_TOOL_EXECUTE_AFTER = HOOK_METHODS.ToolExecuteAfter

export function handleRequest(
  request: JsonRpcMessage,
  context: RequestContext,
): Promise<HandledRequest> | HandledRequest {
  if (request.id === undefined) {
    if (request.method === METHOD_EVENT) {
      return handleEventNotification(request, context)
    }
    return { response: "", shouldExit: false }
  }
  return handleRequestWithResponse(request, context)
}

function handleRequestWithResponse(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> | HandledRequest {
  switch (request.method) {
    case METHOD_INITIALIZE:
      return handleInitialize(request, context)
    case METHOD_SHUTDOWN:
      return handleShutdown(request, context)
    case METHOD_TOOL_CALL:
      return handleToolCall(request, context)
    case METHOD_MESSAGE_USER_BEFORE:
      return handleMessageUserBefore(request, context)
    case METHOD_CHAT_PARAMS:
      return handleChatParams(request, context)
    case METHOD_COMMAND_EXECUTE_BEFORE:
      return handleCommandExecuteBefore(request, context)
    case METHOD_TEXT_COMPLETE:
      return handleTextComplete(request, context)
    case METHOD_PERMISSION_ASK:
      return handlePermissionAsk(request, context)
    case METHOD_PERMISSION_APPROVE:
      return handlePermissionApprove(request, context)
    case METHOD_MODEL_FALLBACK:
      return handleModelFallback(request, context)
    case METHOD_TOOL_EXECUTE_BEFORE:
      return handleToolExecuteBefore(request, context)
    case METHOD_TOOL_EXECUTE_AFTER:
      return handleToolExecuteAfter(request, context)
    default:
      return {
        response: errorResponse(
          request.id,
          ERROR_CODES.METHOD_NOT_FOUND,
          `method not found: ${request.method}`,
        ),
        shouldExit: false,
      }
  }
}

async function handleShutdown(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  await runDisposeHooks(context)
  return { response: okResponse(request.id, {}), shouldExit: true }
}

type DisposeHook = () => unknown

async function runDisposeHooks(context: RequestContext): Promise<void> {
  for (const hook of context.hooks.toReversed()) {
    const dispose = hook.dispose
    if (!isDisposeHook(dispose)) {
      continue
    }
    try {
      await dispose()
    } catch (error) {
      await context.stderr.write(`bun extension dispose: ${errorMessage(error)}\n`)
    }
  }
}

function isDisposeHook(value: unknown): value is DisposeHook {
  return typeof value === "function"
}

export async function runAdapter(options: RuntimeOptions): Promise<void> {
  const context = createRequestContext(options)
  await runStdio(options, (request) => handleRequest(request, context))
}
