import { z } from "zod"

export const JSONRPC_VERSION = "2.0"

export const ERROR_CODES = {
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL_ERROR: -32603,
} as const

const JsonRpcRequestSchema = z
  .object({
    jsonrpc: z.literal(JSONRPC_VERSION),
    id: z.number().int().nonnegative(),
    method: z.string().min(1),
    params: z.unknown().optional().default({}),
  })
  .strict()

export type JsonRpcRequest = z.infer<typeof JsonRpcRequestSchema>

export type ParseRequestResult =
  | { readonly ok: true; readonly request: JsonRpcRequest }
  | { readonly ok: false; readonly message: string }

export function parseJsonRpcRequest(line: string): ParseRequestResult {
  let value: unknown
  try {
    value = JSON.parse(line)
  } catch (error) {
    if (error instanceof SyntaxError) {
      return { ok: false, message: error.message }
    }
    if (error instanceof Error) {
      return { ok: false, message: error.message }
    }
    throw error
  }
  const parsed = JsonRpcRequestSchema.safeParse(value)
  if (!parsed.success) {
    return { ok: false, message: parsed.error.message }
  }
  return { ok: true, request: parsed.data }
}

export function okResponse(id: number, result: unknown): string {
  return `${JSON.stringify({ jsonrpc: JSONRPC_VERSION, id, result })}\n`
}

export function errorResponse(id: number, code: number, message: string): string {
  return `${JSON.stringify({
    jsonrpc: JSONRPC_VERSION,
    id,
    error: { code, message },
  })}\n`
}
