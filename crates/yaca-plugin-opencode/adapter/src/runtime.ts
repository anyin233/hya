import { z } from "zod"

import {
  AdapterOptionsParseError,
  discoverPluginSpecs,
  parseAdapterOptions,
} from "./loader/discovery"
import { runToolExecuteBeforeHooks } from "./hooks"
import { loadLocalPluginHooks, type OpenCodeHooks } from "./loader/init"
import {
  ERROR_CODES,
  errorResponse,
  okResponse,
  parseJsonRpcRequest,
  type JsonRpcRequest,
} from "./protocol"
import { hookRegistrationsFrom } from "./registration"
import {
  buildToolRegistry,
  callRegisteredTool,
  type OpenCodeToolDefinition,
} from "./tool"

export const PROTOCOL_VERSION = 1

const METHOD_INITIALIZE = "initialize"
const METHOD_SHUTDOWN = "shutdown"
const METHOD_TOOL_CALL = "tool/call"
const METHOD_TOOL_EXECUTE_BEFORE = "hook/tool.execute.before"

const InitializeParamsSchema = z
  .object({
    protocol_version: z.literal(PROTOCOL_VERSION),
    host: z.object({
      name: z.string(),
      version: z.string(),
    }),
  })
  .strict()

const ToolCallParamsSchema = z
  .object({
    tool: z.string(),
    session: z.string(),
    call: z.string(),
    input: z.unknown(),
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

export type TextSink = {
  readonly write: (data: string) => unknown
}

export type RuntimeEnv = Readonly<Record<string, string | undefined>>

export type RuntimeOptions = {
  readonly input: ReadableStream<Uint8Array>
  readonly stdout: TextSink
  readonly stderr: TextSink
  readonly version: string
  readonly env?: RuntimeEnv
}

type HandledRequest = {
  readonly response: string
  readonly shouldExit: boolean
}

type RequestContext = {
  readonly version: string
  readonly env: RuntimeEnv
  readonly stderr: TextSink
  readonly hooks: OpenCodeHooks[]
  readonly tools: Map<string, OpenCodeToolDefinition>
}

type LoadedHooksResult =
  | { readonly hooks: readonly OpenCodeHooks[]; readonly response?: undefined }
  | { readonly hooks?: undefined; readonly response: HandledRequest }

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
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> | HandledRequest {
  switch (request.method) {
    case METHOD_INITIALIZE:
      return handleInitialize(request, context)
    case METHOD_SHUTDOWN:
      return { response: okResponse(request.id, {}), shouldExit: true }
    case METHOD_TOOL_CALL:
      return handleToolCall(request, context)
    case METHOD_TOOL_EXECUTE_BEFORE:
      return handleToolExecuteBefore(request, context)
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

export async function runAdapter(options: RuntimeOptions): Promise<void> {
  const context = {
    version: options.version,
    env: options.env ?? process.env,
    stderr: options.stderr,
    hooks: [],
    tools: new Map<string, OpenCodeToolDefinition>(),
  }
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
    await options.stdout.write(handled.response)
    if (handled.shouldExit) {
      break
    }
  }
}

async function handleInitialize(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = InitializeParamsSchema.safeParse(request.params)
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
  const loaded = await loadConfiguredHooks(context, request.id)
  if (loaded.response !== undefined) {
    return loaded.response
  }
  const registry = buildToolRegistry(loaded.hooks)
  context.hooks.splice(0, context.hooks.length, ...loaded.hooks)
  context.tools.clear()
  for (const [name, tool] of registry.tools) {
    context.tools.set(name, tool)
  }
  return {
    response: okResponse(request.id, {
      protocol_version: PROTOCOL_VERSION,
      plugin: {
        id: "opencode",
        version: context.version,
        kind: "opencode",
      },
      hooks: hookRegistrationsFrom(loaded.hooks),
      tools: registry.infos,
    }),
    shouldExit: false,
  }
}

async function handleToolCall(
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
  const directory = context.env.YACA_DIRECTORY ?? process.cwd()
  const worktree = context.env.YACA_WORKTREE ?? directory
  const reply = await callRegisteredTool(context.tools, params.data, {
    directory,
    worktree,
  })
  return {
    response: okResponse(request.id, reply),
    shouldExit: false,
  }
}

async function handleToolExecuteBefore(
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

async function loadConfiguredHooks(
  context: RequestContext,
  id: number,
): Promise<LoadedHooksResult> {
  let options: ReturnType<typeof parseAdapterOptions>
  try {
    options = parseAdapterOptions(context.env.YACA_OPENCODE_OPTIONS_JSON)
  } catch (error) {
    if (error instanceof AdapterOptionsParseError) {
      return {
        response: {
          response: errorResponse(id, ERROR_CODES.INVALID_PARAMS, error.message),
          shouldExit: false,
        },
      }
    }
    throw error
  }
  const directory = context.env.YACA_DIRECTORY ?? process.cwd()
  const worktree = context.env.YACA_WORKTREE ?? directory
  const discovered = await discoverPluginSpecs({
    directory,
    xdgConfigHome: context.env.XDG_CONFIG_HOME,
    home: context.env.HOME,
  })
  const loaded = await loadLocalPluginHooks(
    [...discovered, ...options.plugin],
    pluginInput(context.env, directory, worktree),
  )
  for (const error of loaded.errors) {
    await context.stderr.write(`opencode plugin ${error.spec}: ${error.message}\n`)
  }
  return { hooks: loaded.hooks }
}

function pluginInput(
  env: RuntimeEnv,
  directory: string,
  worktree: string,
): Readonly<Record<string, unknown>> {
  return {
    client: {},
    directory,
    worktree,
    project: {
      id: env.YACA_PROJECT_ID ?? worktree,
      worktree,
      time: Date.now(),
    },
    serverUrl: new URL(env.YACA_SERVER_URL ?? "http://127.0.0.1:0"),
    experimental_workspace: {
      register: () => undefined,
    },
  }
}

function trimTrailingCarriageReturn(line: string): string {
  return line.endsWith("\r") ? line.slice(0, -1) : line
}
