import { HttpError } from "../client"
import { stripTerminalControls } from "../sanitize"

/** Format extension host-command failures as one safe status line. */
export function errorLine(error: unknown): string {
  const text = error instanceof HttpError
    ? error.detail
    : error instanceof TypeError
      ? `unavailable: ${error.message}`
      : error instanceof Error ? error.message : String(error)
  const line = text.split(/\r?\n/).map((part) => stripTerminalControls(part).trim()).find(Boolean)
  return line ?? "unknown error"
}
