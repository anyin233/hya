import {
  handleChatParams,
  handleCommandExecuteBefore,
  handleMessageUserBefore,
  handlePermissionAsk,
  handleToolExecuteAfter,
  handleToolExecuteBefore,
} from "./hook_methods"
import { handleEventNotification } from "./event_method"
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
import { handleTextComplete } from "./text_complete_method"
import { handleToolCall } from "./tool_method"

export { PROTOCOL_VERSION }
export type { RuntimeOptions }

const METHOD_INITIALIZE = "initialize"
const METHOD_SHUTDOWN = "shutdown"
const METHOD_EVENT = "event"
const METHOD_TOOL_CALL = "tool/call"
const METHOD_MESSAGE_USER_BEFORE = "hook/message.user.before"
const METHOD_CHAT_PARAMS = "hook/chat.params"
const METHOD_COMMAND_EXECUTE_BEFORE = "hook/command.execute.before"
const METHOD_TEXT_COMPLETE = "hook/experimental.text.complete"
const METHOD_PERMISSION_ASK = "hook/permission.ask"
const METHOD_TOOL_EXECUTE_BEFORE = "hook/tool.execute.before"
const METHOD_TOOL_EXECUTE_AFTER = "hook/tool.execute.after"

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
      await context.stderr.write(`compat plugin dispose: ${errorMessage(error)}\n`)
    }
  }
}

function isDisposeHook(value: unknown): value is DisposeHook {
  return typeof value === "function"
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

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
