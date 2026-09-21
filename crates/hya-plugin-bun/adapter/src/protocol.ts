export const JSONRPC_VERSION = "2.0"

export const ERROR_CODES = {
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL_ERROR: -32603,
} as const

export type JsonRpcRequest = {
  readonly jsonrpc: "2.0"
  readonly id: number
  readonly method: string
  readonly params: unknown
}

export type JsonRpcNotification = {
  readonly jsonrpc: "2.0"
  readonly id?: undefined
  readonly method: string
  readonly params: unknown
}

export type JsonRpcMessage = JsonRpcRequest | JsonRpcNotification

export type ParseRequestResult =
  | { readonly ok: true; readonly request: JsonRpcMessage }
  | { readonly ok: false; readonly message: string }

const TOP_LEVEL_KEYS = new Set(["jsonrpc", "id", "method", "params"])

export function parseJsonRpcRequest(line: string): ParseRequestResult {
  let value: unknown
  try {
    value = JSON.parse(line)
  } catch (error) {
    if (error instanceof Error) {
      return { ok: false, message: error.message }
    }
    throw error
  }
  if (!isRecordMutable(value)) {
    return { ok: false, message: "request must be a JSON object" }
  }
  for (const key of Object.keys(value)) {
    if (!TOP_LEVEL_KEYS.has(key)) {
      return { ok: false, message: `unknown request field: ${key}` }
    }
  }
  if (value.jsonrpc !== JSONRPC_VERSION) {
    return { ok: false, message: "jsonrpc must be \"2.0\"" }
  }
  if (!isNonEmptyString(value.method)) {
    return { ok: false, message: "method must be a non-empty string" }
  }
  const params = value.params === undefined ? {} : value.params
  if (value.id === undefined) {
    return { ok: true, request: { jsonrpc: JSONRPC_VERSION, method: value.method, params } }
  }
  if (!isNonNegativeInt(value.id)) {
    return { ok: false, message: "id must be a non-negative integer" }
  }
  return {
    ok: true,
    request: { jsonrpc: JSONRPC_VERSION, id: value.id, method: value.method, params },
  }
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

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0
}

function isNonNegativeInt(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0
}

function isRecordMutable(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
