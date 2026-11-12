import { HOOK_METHODS, isHookParams } from "./wire.gen"
import { runStdio } from "./stdio"
export { readLines } from "./stdio"
/**
 * NDJSON JSON-RPC stdio runtime for the Claude adapter.
 *
 * Same wire discipline as the Bun extension adapter (initialize / shutdown /
 * event / tool/call / hook/*), with hook dispatch executing the translated
 * Claude Code shell commands and translating their decisions.
 */

import { claudeHookPayload, claudeToolName, matcherMatches, runClaudeHookCommand, toolDecisionFromClaudeStdout } from "./hooks"
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

export { PROTOCOL_VERSION }
export type { RuntimeOptions }

const METHOD_INITIALIZE = "initialize"
const METHOD_SHUTDOWN = "shutdown"
const METHOD_TOOL_CALL = "tool/call"
const METHOD_MESSAGE_USER_BEFORE = HOOK_METHODS.MessageUserBefore
const METHOD_TOOL_EXECUTE_BEFORE = HOOK_METHODS.ToolExecuteBefore
const METHOD_TOOL_EXECUTE_AFTER = HOOK_METHODS.ToolExecuteAfter
const METHOD_COMPACTION_BEFORE = HOOK_METHODS.CompactionBefore
const METHOD_SESSION_START = HOOK_METHODS.SessionStart
const METHOD_SESSION_END = HOOK_METHODS.SessionEnd
const METHOD_AGENT_SPAWN = HOOK_METHODS.AgentSpawn

/** Dispatch one parsed request or notification. */
export function handleRequest(
  request: JsonRpcMessage,
  context: RequestContext,
): Promise<HandledRequest> | HandledRequest {
  if (request.id === undefined) {
    if (request.method === METHOD_SESSION_START || request.method === METHOD_SESSION_END || request.method === METHOD_AGENT_SPAWN) {
      return handleObservation(request.method, request.params, context)
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
    case METHOD_SESSION_START:
    case METHOD_SESSION_END:
    case METHOD_AGENT_SPAWN:
      return handleObservation(request.method, request.params, context).then(() => ({
        response: okResponse(request.id, {}), shouldExit: false,
      }))
    case METHOD_INITIALIZE:
      return handleInitialize(request, context)
    case METHOD_SHUTDOWN:
      return { response: okResponse(request.id, {}), shouldExit: true }
    case METHOD_TOOL_CALL:
      return {
        response: errorResponse(
          request.id,
          ERROR_CODES.METHOD_NOT_FOUND,
          "the Claude adapter declares no tools in v1; plugin tools arrive through MCP",
        ),
        shouldExit: false,
      }
    case METHOD_TOOL_EXECUTE_BEFORE:
      return handleToolExecuteBefore(request, context)
    case METHOD_TOOL_EXECUTE_AFTER:
      return handleToolExecuteAfter(request, context)
    case METHOD_MESSAGE_USER_BEFORE:
      return handleMessageUserBefore(request, context)
    case METHOD_COMPACTION_BEFORE:
      return handleCompactionBefore(request, context)
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

async function handleObservation(
  method: string,
  params: unknown,
  context: RequestContext,
): Promise<HandledRequest> {
  const wireName = method.slice("hook/".length) as "session.start" | "session.end" | "agent.spawn"
  if (context.translation === undefined || !isHookParams(wireName, params)) {
    return { response: "", shouldExit: false }
  }
  const groups = context.translation.hookGroups[wireName] ?? []
  const record = params
  for (const group of groups) {
    for (const hook of group.commands) {
      await runClaudeHookCommand(hook, claudeHookPayload(wireName, {
        session: "session" in record ? record.session : record.parent,
      }), context.bundleRoot)
    }
  }
  return { response: "", shouldExit: false }
}

async function handleCompactionBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  if (context.translation === undefined || !isHookParams("compaction.before", request.params)) {
    return invalidParams(request.id, "params must be an object")
  }
  const params = request.params
  for (const group of context.translation.hookGroups["compaction.before"] ?? []) {
    for (const hook of group.commands) {
      await runClaudeHookCommand(hook, claudeHookPayload("compaction.before", {
        session: params.session,
      }), context.bundleRoot)
    }
  }
  return { response: okResponse(request.id, { outcome: "proceed" }), shouldExit: false }
}

async function handleToolExecuteBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = request.params
  if (!isHookParams("tool.execute.before", params)) {
    return invalidParams(request.id, "invalid tool.execute.before params")
  }
  if (context.translation === undefined) {
    return invalidParams(request.id, "adapter is not initialized")
  }
  const hooks = context.translation.hookGroups?.["tool.execute.before"] ?? []
  const toolName = claudeToolName(params.tool)
  for (const group of hooks) {
    if (!matcherMatches(group.matcher, toolName)) {
      continue
    }
    for (const hook of group.commands) {
      let run
      try {
        run = await runClaudeHookCommand(
          hook,
          claudeHookPayload("tool.execute.before", {
            session: params.session,
            tool: toolName,
            input: params.input,
          }),
          context.bundleRoot,
        )
      } catch (error) {
        return { response: okResponse(request.id, { outcome: "veto", reason: `Claude guard failed: ${String(error)}` }), shouldExit: false }
      }
      const decision = toolDecisionFromClaudeStdout(run.stdout)
      if (decision !== undefined && decision.outcome === "veto") {
        return { response: okResponse(request.id, decision), shouldExit: false }
      }
    }
  }
  return {
    response: okResponse(request.id, { outcome: "continue", input: params.input }),
    shouldExit: false,
  }
}

async function handleToolExecuteAfter(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = request.params
  if (!isHookParams("tool.execute.after", params)) {
    return invalidParams(request.id, "invalid tool.execute.after params")
  }
  if (context.translation === undefined) {
    return invalidParams(request.id, "adapter is not initialized")
  }
  const hooks = context.translation.hookGroups?.["tool.execute.after"] ?? []
  const toolName = claudeToolName(params.tool)
  for (const group of hooks) {
    if (!matcherMatches(group.matcher, toolName)) {
      continue
    }
    for (const hook of group.commands) {
      await runClaudeHookCommand(
        hook,
        claudeHookPayload("tool.execute.after", {
          session: params.session,
          tool: toolName,
          input: params.input,
          result: params.result,
        }),
        context.bundleRoot,
      )
    }
  }
  return {
    response: okResponse(request.id, {
      outcome: "continue",
      result: params.result,
    }),
    shouldExit: false,
  }
}

async function handleMessageUserBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = request.params
  if (!isHookParams("message.user.before", params)) {
    return invalidParams(request.id, "invalid message.user.before params")
  }
  if (context.translation === undefined) {
    return invalidParams(request.id, "adapter is not initialized")
  }
  const hooks = context.translation.hookGroups?.["message.user.before"] ?? []
  for (const group of hooks) {
    for (const hook of group.commands) {
      await runClaudeHookCommand(
        hook,
        claudeHookPayload("message.user.before", {
          session: params.session,
        }),
        context.bundleRoot,
      )
    }
  }
  // The v1 wire outcome for message.user.before is continue-only; CC
  // UserPromptSubmit blocks are advisory until the wire gains a veto variant.
  return {
    response: okResponse(request.id, { outcome: "continue", text: params.text }),
    shouldExit: false,
  }
}

function invalidParams(id: number, message: string): HandledRequest {
  return { response: errorResponse(id, ERROR_CODES.INVALID_PARAMS, message), shouldExit: false }
}

/** Run the adapter loop over one input stream. */
export async function runAdapter(options: RuntimeOptions): Promise<void> {
  const context = createRequestContext(options)
  await runStdio(options, (request) => handleRequest(request, context))
}
