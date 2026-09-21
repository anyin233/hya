/** Small runtime validation helpers shared by the adapter modules. */

/** Narrow `unknown` to a non-array object. */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

/** Narrow `unknown` to a non-empty string. */
export function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0
}

/** Result shape shared by the params validators. */
export type ValidationResult<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly message: string }

/** Check that every required string key is present on the params object. */
export function recordWithStrings(
  value: unknown,
  required: readonly string[],
): ValidationResult<Record<string, unknown>> {
  if (!isRecord(value)) {
    return { ok: false, message: "params must be an object" }
  }
  for (const key of required) {
    if (!isNonEmptyString(value[key])) {
      return { ok: false, message: `params.${key} must be a non-empty string` }
    }
  }
  return { ok: true, value }
}

/** Wrap a value in a success validation result. */
export function ok<T>(value: T): ValidationResult<T> {
  return { ok: true, value }
}
