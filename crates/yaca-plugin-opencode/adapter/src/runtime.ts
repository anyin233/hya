import { z } from "zod"

import {
  ERROR_CODES,
  errorResponse,
  okResponse,
  parseJsonRpcRequest,
  type JsonRpcRequest,
} from "./protocol"

export const PROTOCOL_VERSION = 1

const METHOD_INITIALIZE = "initialize"
const METHOD_SHUTDOWN = "shutdown"

const InitializeParamsSchema = z
  .object({
    protocol_version: z.literal(PROTOCOL_VERSION),
    host: z.object({
      name: z.string(),
      version: z.string(),
    }),
  })
  .strict()

export type TextSink = {
  readonly write: (data: string) => unknown
}

export type RuntimeOptions = {
  readonly input: ReadableStream<Uint8Array>
  readonly stdout: TextSink
  readonly stderr: TextSink
  readonly version: string
}

type HandledRequest = {
  readonly response: string
  readonly shouldExit: boolean
}

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
  version: string,
): HandledRequest {
  switch (request.method) {
    case METHOD_INITIALIZE:
      return handleInitialize(request, version)
    case METHOD_SHUTDOWN:
      return { response: okResponse(request.id, {}), shouldExit: true }
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
  for await (const line of readLines(options.input)) {
    if (line.length === 0) {
      continue
    }
    const parsed = parseJsonRpcRequest(line)
    if (!parsed.ok) {
      await options.stderr.write(`invalid JSON-RPC request: ${parsed.message}\n`)
      continue
    }
    const handled = handleRequest(parsed.request, options.version)
    await options.stdout.write(handled.response)
    if (handled.shouldExit) {
      break
    }
  }
}

function handleInitialize(
  request: JsonRpcRequest,
  version: string,
): HandledRequest {
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
  return {
    response: okResponse(request.id, {
      protocol_version: PROTOCOL_VERSION,
      plugin: {
        id: "opencode",
        version,
        kind: "opencode",
      },
      hooks: [],
      tools: [],
    }),
    shouldExit: false,
  }
}

function trimTrailingCarriageReturn(line: string): string {
  return line.endsWith("\r") ? line.slice(0, -1) : line
}
