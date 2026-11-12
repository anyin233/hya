import { runChatParamsHooks } from "./chat_params_hooks"
import { runToolExecuteAfterHooks, runToolExecuteBeforeHooks } from "./hooks"
import { runModelFallbackHooks } from "./model_fallback_hooks"
import { runPermissionApproveHooks } from "./permission_approve"
import { runPermissionAskHooks } from "./permission_hooks"
import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import { runTextHooks } from "./text_hooks"
import type { ExtensionHooks } from "./loader/init"
import type { HandledRequest, RequestContext } from "./runtime_types"
import { isHookParams, type HookName, type HookParams } from "./wire.gen"

async function invoke<K extends HookName>(
  request: JsonRpcRequest,
  context: RequestContext,
  name: K,
  run: (hooks: readonly ExtensionHooks[], params: HookParams[K]) => Promise<unknown>,
): Promise<HandledRequest> {
  if (!isHookParams(name, request.params)) {
    return { response: errorResponse(request.id, ERROR_CODES.INVALID_PARAMS, `invalid ${name} params`), shouldExit: false }
  }
  return { response: okResponse(request.id, await run(context.hooks, request.params)), shouldExit: false }
}

export const handleMessageUserBefore = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "message.user.before", (hooks, params) => runTextHooks(hooks, "message.user.before", params))
export const handleCommandExecuteBefore = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "command.execute.before", (hooks, params) => runTextHooks(hooks, "command.execute.before", params))
export const handleTextComplete = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "experimental.text.complete", (hooks, params) => runTextHooks(hooks, "experimental.text.complete", params))
export const handleChatParams = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "chat.params", runChatParamsHooks)
export const handlePermissionAsk = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "permission.ask", runPermissionAskHooks)
export const handlePermissionApprove = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "permission.approve", runPermissionApproveHooks)
export const handleModelFallback = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "model.fallback", runModelFallbackHooks)
export const handleToolExecuteBefore = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "tool.execute.before", runToolExecuteBeforeHooks)
export const handleToolExecuteAfter = (request: JsonRpcRequest, context: RequestContext) =>
  invoke(request, context, "tool.execute.after", runToolExecuteAfterHooks)
