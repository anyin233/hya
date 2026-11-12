/** Helpers for initialize and untyped extension declarations. */

export type ValidationResult<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly message: string }

export function ok<T>(value: T): ValidationResult<T> {
  return { ok: true, value }
}

export function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

export function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0
}
