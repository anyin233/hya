/**
 * The v1 HTTP operation catalog used by `/api` and its completion.
 *
 * `operations.json` is generated next to `docs/protocol/openapi.json` by
 * `cargo run -p xtask -- gen-api` (test/api-catalog.test.ts checks they
 * agree). It lives inside the package because the TUI ships on its own under
 * `lib/hya/tui`, where the repository's docs do not exist.
 */
import catalog from "./operations.json"

interface Operation {
  method: string
  path: string
  operationId: string
  streaming: boolean
}

const catalogOperations = catalog as Operation[]

/** One line per operation: method, path, operation id, streaming marker. */
export function operations(): string {
  return catalogOperations.map((op) =>
    `${op.method.padEnd(6)} ${op.path}  ${op.operationId}${op.streaming ? " [stream]" : ""}`,
  ).sort().join("\n")
}

/** `METHOD /v1/path` names for completion. */
export const apiOperationNames = catalogOperations.map((op) => `${op.method} ${op.path}`)

/** Pretty JSON, truncated for display. */
export function brief(value: unknown): string {
  const text = JSON.stringify(value, null, 2) ?? "null"
  return text.length > 20_000 ? `${text.slice(0, 20_000)}\n… output truncated` : text
}
