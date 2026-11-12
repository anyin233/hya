/** Shared stdio runtime. `xtask gen-plugin` checks the Claude adapter copy. */
import { ERROR_CODES, errorResponse, parseJsonRpcRequest, type JsonRpcMessage } from "./protocol"

export type TextSink = { readonly write: (data: string) => unknown }
export type HandledRequest = { readonly response: string; readonly shouldExit: boolean }

export async function* readLines(input: ReadableStream<Uint8Array>): AsyncGenerator<string> {
  const decoder = new TextDecoder()
  let buffered = ""
  for await (const chunk of input) {
    buffered += decoder.decode(chunk, { stream: true })
    let newline = buffered.indexOf("\n")
    while (newline >= 0) {
      yield buffered.slice(0, newline).replace(/\r$/, "")
      buffered = buffered.slice(newline + 1)
      newline = buffered.indexOf("\n")
    }
  }
  const tail = buffered + decoder.decode()
  if (tail.length > 0) yield tail.replace(/\r$/, "")
}

/** Initialize is a barrier. Shutdown stops admission and drains before disposal. */
export async function runStdio(
  options: { readonly input: ReadableStream<Uint8Array>; readonly stdout: TextSink; readonly stderr: TextSink },
  handle: (request: JsonRpcMessage) => HandledRequest | Promise<HandledRequest>,
): Promise<void> {
  const active = new Set<Promise<void>>()
  const ids = new Set<number>()
  let initialized = false
  let output = Promise.resolve()
  const write = (line: string) => {
    output = output.then(async () => { if (line.length > 0) await options.stdout.write(line) })
    return output
  }
  const dispatch = async (request: JsonRpcMessage) => {
    let handled: HandledRequest
    try {
      handled = await handle(request)
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      if (request.id !== undefined) {
        await write(errorResponse(request.id, ERROR_CODES.INTERNAL_ERROR, message))
      } else {
        await options.stderr.write(`notification ${request.method}: ${message}\n`)
      }
      return
    }
    await write(handled.response)
  }
  for await (const line of readLines(options.input)) {
    if (line.length === 0) continue
    const parsed = parseJsonRpcRequest(line)
    if (!parsed.ok) {
      await options.stderr.write(`invalid JSON-RPC request: ${parsed.message}\n`)
      continue
    }
    const request = parsed.request
    if (request.id !== undefined && request.method === "initialize") {
      if (initialized) {
        await write(errorResponse(request.id, ERROR_CODES.INVALID_PARAMS, "adapter already initialized"))
        continue
      }
      initialized = true
      await dispatch(request)
      continue
    }
    if (request.id !== undefined && request.method === "shutdown") {
      await Promise.all(active)
      await dispatch(request)
      break
    }
    if (request.id !== undefined && ids.has(request.id)) {
      await write(errorResponse(request.id, ERROR_CODES.INVALID_PARAMS, "duplicate active request id"))
      continue
    }
    if (request.id !== undefined) ids.add(request.id)
    const job = dispatch(request).finally(() => {
      active.delete(job)
      if (request.id !== undefined) ids.delete(request.id)
    })
    active.add(job)
  }
  await Promise.all(active)
  await output
}
