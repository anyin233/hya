/** Hand-rolled param validation helpers (the adapter carries no schema dep). */

export type ValidationResult<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly message: string }

export function ok<T>(value: T): ValidationResult<T> {
  return { ok: true, value }
}

export function err<T>(message: string): ValidationResult<T> {
  return { ok: false, message }
}

export function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

export function isRecordMutable(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

export function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0
}

export function isInt(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value)
}

export function isNonNegativeInt(value: unknown): value is number {
  return isInt(value) && value >= 0
}

function field(
  record: Readonly<Record<string, unknown>>,
  key: string,
  check: (value: unknown) => boolean,
  label: string,
): string | undefined {
  const value = record[key]
  if (!check(value)) {
    return `params.${label} is invalid`
  }
  return undefined
}

/**
 * Validate a params object with required string fields. Unknown fields are
 * tolerated so the host can evolve payloads without breaking the adapter.
 */
export function recordWithStrings(
  value: unknown,
  keys: readonly string[],
  optionalKeys: readonly string[] = [],
): ValidationResult<Record<string, unknown>> {
  if (!isRecord(value)) {
    return err("params must be an object")
  }
  for (const key of keys) {
    const problem = field(value, key, isNonEmptyString, key)
    if (problem !== undefined) {
      return err(problem)
    }
  }
  for (const key of optionalKeys) {
    if (value[key] === undefined) {
      continue
    }
    const problem = field(value, key, isNonEmptyString, key)
    if (problem !== undefined) {
      return err(problem)
    }
  }
  return ok({ ...value })
}
