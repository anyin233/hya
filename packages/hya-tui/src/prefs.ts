/**
 * TUI preferences (docs/tui.md "Preferences file"): a small JSON object in
 * `$HYA_TUI_CONFIG`, else `$XDG_CONFIG_HOME/hya/tui.json`, else
 * `~/.config/hya/tui.json`. The TUI reads it once at start and writes it
 * when a preference changes (`/theme`).
 *
 * - Missing file: no preferences, no warning.
 * - Unreadable or corrupt file (not a JSON object): no preferences, and a
 *   warning naming the file; the next save replaces it.
 * - A known key with a value of the wrong type is ignored.
 * - Saving merges the changed keys into what is on disk (unknown keys,
 *   e.g. from a newer TUI, are kept) and writes a temporary file in the
 *   same directory, then renames it over the file (atomic).
 *
 * Add a preference by adding a field to `TuiPreferences` and a validator to
 * `validators`.
 */
import { mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"

/** The known preference keys. Every key is optional; unset means the built-in default. */
export interface TuiPreferences {
  /** Built-in theme name (`hya`, `light`, `contrast`, `ember`); default `hya`. */
  theme?: string
}

type Validators = { [Key in keyof Required<TuiPreferences>]: (value: unknown) => value is TuiPreferences[Key] }

const validators: Validators = {
  theme: (value): value is string => typeof value === "string" && value.length > 0,
}

/** The environment variable that points the TUI at another preferences file (tests, several profiles). */
export const preferencesEnv = "HYA_TUI_CONFIG"

/** `$HYA_TUI_CONFIG`, else `$XDG_CONFIG_HOME/hya/tui.json`, else `$HOME/.config/hya/tui.json`. */
export function preferencesPath(env: Record<string, string | undefined>): string {
  const override = env[preferencesEnv]
  if (override) return override
  const base = env.XDG_CONFIG_HOME || join(env.HOME ?? ".", ".config")
  return join(base, "hya", "tui.json")
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

/** The raw JSON object on disk; `undefined` when missing, `null` when unreadable or not an object. */
function readRaw(path: string): Record<string, unknown> | undefined | null {
  let text: string
  try {
    text = readFileSync(path, "utf8")
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "ENOENT" ? undefined : null
  }
  try {
    const value: unknown = JSON.parse(text)
    return isObject(value) ? value : null
  } catch {
    return null
  }
}

export interface LoadedPreferences {
  preferences: TuiPreferences
  /** Set when the file exists but could not be used. */
  warning?: string
}

/** Read the preferences file; never throws. */
export function loadPreferences(path: string): LoadedPreferences {
  const raw = readRaw(path)
  if (raw === undefined) return { preferences: {} }
  if (raw === null) return { preferences: {}, warning: `Ignored unreadable TUI preferences ${path}` }
  const preferences: Record<string, unknown> = {}
  for (const [key, valid] of Object.entries(validators) as [string, (value: unknown) => boolean][]) {
    if (key in raw && valid(raw[key])) preferences[key] = raw[key]
  }
  return { preferences: preferences as TuiPreferences }
}

let writes = 0

/** Merge `patch` into the file (keeping unknown keys) and write it atomically; throws on I/O errors. */
export function savePreferences(path: string, patch: Partial<TuiPreferences>): void {
  const current = readRaw(path) ?? {}
  const next = { ...current, ...patch }
  mkdirSync(dirname(path), { recursive: true })
  const temp = join(dirname(path), `.tui.json.${process.pid}.${++writes}.tmp`)
  try {
    writeFileSync(temp, `${JSON.stringify(next, null, 2)}\n`)
    renameSync(temp, path)
  } catch (error) {
    rmSync(temp, { force: true })
    throw error
  }
}
