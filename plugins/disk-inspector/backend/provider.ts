import { hostname } from "node:os";
import { CONTRACT_VERSION, PLUGIN_ID, type InspectorInfo } from "../shared/contracts";

const record = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

export function inspectorInfo(): InspectorInfo {
  return {
    contractVersion: CONTRACT_VERSION,
    machine: { hostname: hostname(), platform: process.platform },
    capabilities: { volumes: false, scans: false, cancel: false },
  };
}

export const initialization = {
  protocol_version: 1,
  plugin: { id: PLUGIN_ID, version: "0.0.0", kind: "bun" },
  tools: [],
  hooks: [],
  apis: [{ name: "info", description: "Provider identity and implemented capabilities" }],
};

/** Pure dispatch makes the stdio provider testable without booting a backend. */
export function dispatch(message: unknown): { frame?: unknown; shutdown: boolean } {
  if (!record(message) || message.jsonrpc !== "2.0" || typeof message.method !== "string") {
    return {
      frame: { jsonrpc: "2.0", id: null, error: { code: -32600, message: "Invalid Request" } },
      shutdown: false,
    };
  }
  const id = message.id;
  if (id === undefined) return { shutdown: message.method === "shutdown" };
  if (typeof id !== "string" && (typeof id !== "number" || !Number.isSafeInteger(id))) {
    return {
      frame: { jsonrpc: "2.0", id: null, error: { code: -32600, message: "Invalid request id" } },
      shutdown: false,
    };
  }
  const reply = (result: unknown, shutdown = false) => ({
    frame: { jsonrpc: "2.0", id, result }, shutdown,
  });
  switch (message.method) {
    case "initialize":
      if (!record(message.params) || message.params.protocol_version !== 1) {
        return { frame: { jsonrpc: "2.0", id, error: { code: -32602, message: "Expected protocol_version 1" } }, shutdown: false };
      }
      return reply(initialization);
    case "shutdown":
      return reply({}, true);
    case "api/request":
      if (!record(message.params) || message.params.api !== "info" || message.params.method !== "GET" || message.params.path !== "/info") {
        return reply({ status: 404, body: { error: { code: "endpoint_not_found", message: "Only GET /info is implemented" } } });
      }
      return reply({ status: 200, body: inspectorInfo() });
    default:
      return { frame: { jsonrpc: "2.0", id, error: { code: -32601, message: "Method not found" } }, shutdown: false };
  }
}
