import { expect, test } from "bun:test"

import {
  ERROR_CODES,
  errorResponse,
  okResponse,
  parseJsonRpcRequest,
} from "../src/protocol"

test("parses valid JSON-RPC requests", () => {
  const parsed = parseJsonRpcRequest(
    '{"jsonrpc":"2.0","id":42,"method":"initialize","params":{"protocol_version":1}}',
  )

  expect(parsed.ok).toBe(true)
  if (parsed.ok) {
    expect(parsed.request.id).toBe(42)
    expect(parsed.request.method).toBe("initialize")
  }
})

test("parses valid JSON-RPC notifications", () => {
  const parsed = parseJsonRpcRequest(
    '{"jsonrpc":"2.0","method":"event","params":{"envelope":{"seq":1}}}',
  )

  expect(parsed.ok).toBe(true)
  if (parsed.ok) {
    expect(parsed.request.id).toBeUndefined()
    expect(parsed.request.method).toBe("event")
  }
})

test("rejects malformed JSON-RPC request lines", () => {
  const parsed = parseJsonRpcRequest("{not-json")

  expect(parsed.ok).toBe(false)
  if (!parsed.ok) {
    expect(parsed.message.length).toBeGreaterThan(0)
  }
})

test("rejects structurally invalid JSON-RPC requests", () => {
  expect(parseJsonRpcRequest('{"jsonrpc":"1.0","id":1,"method":"x"}').ok).toBe(false)
  expect(parseJsonRpcRequest('{"jsonrpc":"2.0","id":-1,"method":"x"}').ok).toBe(false)
  expect(parseJsonRpcRequest('{"jsonrpc":"2.0","id":1.5,"method":"x"}').ok).toBe(false)
  expect(parseJsonRpcRequest('{"jsonrpc":"2.0","id":1,"method":""}').ok).toBe(false)
  expect(parseJsonRpcRequest('{"jsonrpc":"2.0","id":1,"method":"x","extra":1}').ok).toBe(
    false,
  )
  expect(parseJsonRpcRequest('["jsonrpc"]').ok).toBe(false)
})

test("serializes success and error responses", () => {
  expect(okResponse(7, { ready: true })).toBe(
    '{"jsonrpc":"2.0","id":7,"result":{"ready":true}}\n',
  )
  expect(errorResponse(8, ERROR_CODES.METHOD_NOT_FOUND, "missing")).toBe(
    '{"jsonrpc":"2.0","id":8,"error":{"code":-32601,"message":"missing"}}\n',
  )
})
