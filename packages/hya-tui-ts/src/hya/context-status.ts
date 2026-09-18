/**
 * Runtime decode + presentation for the session `context` block: the latest
 * token-accounting occupancy report from the backend.
 *
 * The field is additive on the session JSON, so decoding is tolerant — an
 * absent or malformed value yields `undefined` and callers fall back to the
 * legacy last-message computation.
 */

export type ContextStatus = {
  tokens: number
  source: "provider" | "estimate"
  mode: "auto" | "provider" | "estimate"
  threshold: number
}

export type ContextStatusView = {
  /** Localized occupancy figure. */
  tokens: string
  /** Localized threshold the occupancy is judged against. */
  limit: string
  /** Occupancy as a percentage of the threshold, clamped to 100. */
  percent: number
  /** Provenance badge; set when the figure is a local estimate. */
  badge: string | null
}

const numberFormat = new Intl.NumberFormat("en-US")

/** Decode a session `context` block; `undefined` when absent or malformed. */
export function parseContextStatus(value: unknown): ContextStatus | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined
  const input = value as Record<string, unknown>
  const tokens = nonNegativeInteger(input.tokens)
  const threshold = nonNegativeInteger(input.threshold)
  if (tokens === undefined || threshold === undefined) return undefined
  if (input.source !== "provider" && input.source !== "estimate") return undefined
  if (input.mode !== "auto" && input.mode !== "provider" && input.mode !== "estimate") return undefined
  return { tokens, source: input.source, mode: input.mode, threshold }
}

/** Present occupancy against the threshold with a provenance badge. */
export function presentContextStatus(status: ContextStatus): ContextStatusView {
  const percent = status.threshold > 0 ? Math.min(100, Math.round((status.tokens / status.threshold) * 100)) : 100
  return {
    tokens: numberFormat.format(status.tokens),
    limit: numberFormat.format(status.threshold),
    percent,
    badge: status.source === "estimate" ? "estimated" : null,
  }
}

function nonNegativeInteger(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : undefined
}
