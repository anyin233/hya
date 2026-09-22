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
  parseJsonRpcRequest,
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
const METHOD_MESSAGE_USER_BEFORE = "hook/message.user.before"
const METHOD_TOOL_EXECUTE_BEFORE = "hook/tool.execute.before"
const METHOD_TOOL_EXECUTE_AFTER = "hook/tool.execute.after"
const METHOD_COMPACTION_BEFORE = "hook/compaction.before"
const METHOD_SESSION_START = "hook/session.start"
const METHOD_SESSION_END = "hook/session.end"
const METHOD_AGENT_SPAWN = "hook/agent.spawn"

/** Read newline-delimited text from a byte stream. */
export async function* readLines(
  input: ReadableStream<Uint8Array>,
): AsyncGenerator<string> {
  const decoder = new TextDecoder()
  let buffered = ""
  for await (const chunk of input) {
    buffered += decoder.decode(chunk, { stream: true })
    let newline = buffered.indexOf("\n")
    while (newline >= 0) {
      yield trimTrailingCarriageReturn(buffered.slice(0, newline))
      buffered = buffered.slice(newline + 1)
      newline = buffered.indexOf("\n")
    }
  }
  const tail = buffered + decoder.decode()
  if (tail.length > 0) {
    yield trimTrailingCarriageReturn(tail)
  }
}

/** Dispatch one parsed request (or notification) against the context. */
export function handleRequest(
  request: JsonRpcMessage,
  context: RequestContext,
): Promise<HandledRequest> | HandledRequest {
  if (request.id === undefined) {
    if ([METHOD_SESSION_START, METHOD_SESSION_END, METHOD_AGENT_SPAWN].includes(request.method)) {
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
  if (context.translation === undefined || typeof params !== "object" || params === null || Array.isArray(params)) {
    return { response: "", shouldExit: false }
  }
  const wireName = method.slice("hook/".length) as "session.start" | "session.end" | "agent.spawn"
  const groups = context.translation.hookGroups[wireName] ?? []
  const record = params as Record<string, unknown>
  for (const group of groups) {
    for (const hook of group.commands) {
      await runClaudeHookCommand(hook, claudeHookPayload(wireName, {
        session: typeof record["session"] === "string"
          ? record["session"]
          : typeof record["parent"] === "string" ? record["parent"] : "",
      }), context.bundleRoot)
    }
  }
  return { response: "", shouldExit: false }
}

async function handleCompactionBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  if (context.translation === undefined || typeof request.params !== "object" || request.params === null || Array.isArray(request.params)) {
    return invalidParams(request.id, "params must be an object")
  }
  const params = request.params as Record<string, unknown>
  for (const group of context.translation.hookGroups["compaction.before"] ?? []) {
    for (const hook of group.commands) {
      await runClaudeHookCommand(hook, claudeHookPayload("compaction.before", {
        session: typeof params["session"] === "string" ? params["session"] : "",
      }), context.bundleRoot)
    }
  }
  return { response: okResponse(request.id, { outcome: "proceed" }), shouldExit: false }
}

async function handleToolExecuteBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateToolParams(request.params, ["session", "message", "call", "tool"])
  if (!params.ok) {
    return invalidParams(request.id, params.message)
  }
  if (context.translation === undefined) {
    return invalidParams(request.id, "adapter is not initialized")
  }
  const hooks = context.translation.hookGroups?.["tool.execute.before"] ?? []
  const toolName = claudeToolName(params.value["tool"] as string)
  for (const group of hooks) {
    if (!matcherMatches(group.matcher, toolName)) {
      continue
    }
    for (const hook of group.commands) {
      const run = await runClaudeHookCommand(
        hook,
        claudeHookPayload("tool.execute.before", {
          session: params.value["session"] as string,
          tool: toolName,
          input: params.value["input"],
        }),
        context.bundleRoot,
      )
      const decision = toolDecisionFromClaudeStdout(run.stdout)
      if (decision !== undefined && decision.outcome === "veto") {
        return { response: okResponse(request.id, decision), shouldExit: false }
      }
    }
  }
  return {
    response: okResponse(request.id, { outcome: "continue", input: params.value["input"] }),
    shouldExit: false,
  }
}

async function handleToolExecuteAfter(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateToolParams(request.params, ["session", "message", "call", "tool"])
  if (!params.ok) {
    return invalidParams(request.id, params.message)
  }
  if (context.translation === undefined) {
    return invalidParams(request.id, "adapter is not initialized")
  }
  const hooks = context.translation.hookGroups?.["tool.execute.after"] ?? []
  const toolName = claudeToolName(params.value["tool"] as string)
  for (const group of hooks) {
    if (!matcherMatches(group.matcher, toolName)) {
      continue
    }
    for (const hook of group.commands) {
      await runClaudeHookCommand(
        hook,
        claudeHookPayload("tool.execute.after", {
          session: params.value["session"] as string,
          tool: toolName,
          input: params.value["input"],
          result: params.value["result"],
        }),
        context.bundleRoot,
      )
    }
  }
  return {
    response: okResponse(request.id, {
      outcome: "continue",
      result: params.value["result"],
    }),
    shouldExit: false,
  }
}

async function handleMessageUserBefore(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateToolParams(request.params, ["session", "text"])
  if (!params.ok) {
    return invalidParams(request.id, params.message)
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
          session: params.value["session"] as string,
        }),
        context.bundleRoot,
      )
    }
  }
  // The v1 wire outcome for message.user.before is continue-only; CC
  // UserPromptSubmit blocks are advisory until the wire gains a veto variant.
  return {
    response: okResponse(request.id, { outcome: "continue", text: params.value["text"] }),
    shouldExit: false,
  }
}

function validateToolParams(
  value: unknown,
  required: readonly string[],
): { ok: true; value: Record<string, unknown> } | { ok: false; message: string } {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return { ok: false, message: "params must be an object" }
  }
  const record = value as Record<string, unknown>
  for (const key of required) {
    if (typeof record[key] !== "string" || (record[key] as string).length === 0) {
      return { ok: false, message: `params.${key} must be a non-empty string` }
    }
  }
  if (!("input" in record) && required.includes("tool")) {
    return { ok: false, message: "params.input is required" }
  }
  return { ok: true, value: record }
}

function invalidParams(id: number, message: string): HandledRequest {
  return { response: errorResponse(id, ERROR_CODES.INVALID_PARAMS, message), shouldExit: false }
}

/** Run the adapter loop over one input stream. */
export async function runAdapter(options: RuntimeOptions): Promise<void> {
  const context = createRequestContext(options)
  for await (const line of readLines(options.input)) {
    if (line.length === 0) {
      continue
    }
    const parsed = parseJsonRpcRequest(line)
    if (!parsed.ok) {
      await options.stderr.write(`invalid JSON-RPC request: ${parsed.message}\n`)
      continue
    }
    const handled = await handleRequest(parsed.request, context)
    if (handled.response.length > 0) {
      await options.stdout.write(handled.response)
    }
    if (handled.shouldExit) {
      break
    }
  }
}

function trimTrailingCarriageReturn(line: string): string {
  return line.endsWith("\r") ? line.slice(0, -1) : line
}
