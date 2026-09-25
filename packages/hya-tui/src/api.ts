/** The v1 HTTP operation catalog used by `/api` and its completion. */
import openapi from "../../../docs/protocol/openapi.json"

type Paths = Record<string, Record<string, { operationId?: string; "x-server-streaming"?: boolean }>>

/** One line per operation: method, path, operation id, streaming marker. */
export function operations(): string {
  const paths = openapi.paths as Paths
  return Object.entries(paths).flatMap(([path, methods]) =>
    Object.entries(methods).map(([method, detail]) =>
      `${method.toUpperCase().padEnd(6)} ${path}  ${detail.operationId ?? ""}${detail["x-server-streaming"] ? " [stream]" : ""}`,
    ),
  ).sort().join("\n")
}

/** `METHOD /v1/path` names for completion. */
export const apiOperationNames = Object.entries(openapi.paths as Record<string, Record<string, unknown>>)
  .flatMap(([path, methods]) => Object.keys(methods).map((method) => `${method.toUpperCase()} ${path}`))

/** Pretty JSON, truncated for display. */
export function brief(value: unknown): string {
  const text = JSON.stringify(value, null, 2) ?? "null"
  return text.length > 20_000 ? `${text.slice(0, 20_000)}\n… output truncated` : text
}
