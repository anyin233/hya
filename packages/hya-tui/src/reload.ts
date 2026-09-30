/**
 * TUI hot update (docs/tui.md "Hot update after `hya serve restart`"): the
 * app runs as a child of a small supervisor (src/supervisor.ts, started by
 * src/main.ts). After `hya serve restart` hands the backend to its successor,
 * the app writes a reload request to the file the supervisor named in
 * `HYA_TUI_RELOAD_FILE`, restores the terminal, and exits with
 * `reloadExitCode`; the supervisor then starts the app again from the files
 * on disk, so it runs the TUI code installed next to the new backend.
 *
 * The request carries the next command line (the open session instead of the
 * first start's `--continue`/`--resume`) and the unsent composer draft; the
 * next app finds the draft in `HYA_TUI_RELOAD`.
 *
 * Only `node:fs` here: the supervisor loads this module and must stay light.
 */
import { writeFileSync } from "node:fs"

/** The app asks its supervisor to start it again (with a request in the reload file). */
export const reloadExitCode = 75
/** Where the app writes its reload request; set by the supervisor, so its presence marks the app process. */
export const reloadFileEnv = "HYA_TUI_RELOAD_FILE"
/** The supervisor's pid: the app exits when its parent is no longer it. */
export const supervisorEnv = "HYA_TUI_SUPERVISOR"
/** `ReloadState` JSON the supervisor hands to a reloaded app. */
export const reloadStateEnv = "HYA_TUI_RELOAD"

export interface Draft {
  text: string
  cursor: number
}

/** What a reloading app asks its supervisor for. */
export interface ReloadRequest {
  /** The next app's command line (after the entry script). */
  argv: string[]
  draft?: Draft
}

/** What a reloaded app starts with. */
export interface ReloadState {
  draft?: Draft
}

export interface Supervision {
  /** The reload request file. */
  file: string
  /** The supervisor's pid. */
  parent: number
}

/**
 * The command line of the reloaded app: `argv` without its startup choice
 * (`--continue`, `--session`, `--resume [id]`), plus `--session <session>`
 * when one is open; `--server`, when given, becomes `server`.
 */
export function reloadArguments(argv: readonly string[], next: { session?: string; server?: string }): string[] {
  const out: string[] = []
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index]!
    const value = argv[index + 1]
    if (arg === "--continue" || arg === "-c") continue
    if (arg === "--session" || arg === "-s") { index++; continue }
    if (arg === "--resume") {
      // The id is optional: a following flag is not one (src/cli.ts).
      if (value !== undefined && !value.startsWith("-")) index++
      continue
    }
    if (arg === "--server" && value !== undefined) {
      out.push(arg, next.server ?? value)
      index++
      continue
    }
    out.push(arg)
  }
  if (next.session) out.push("--session", next.session)
  return out
}

/** Write the request the supervisor reads after the app exits with `reloadExitCode`. */
export function requestReload(file: string, request: ReloadRequest): void {
  writeFileSync(file, JSON.stringify(request), { mode: 0o600 })
}

/** Parse a reload request; undefined when it is not one. */
export function parseReloadRequest(text: string): ReloadRequest | undefined {
  try {
    const value = JSON.parse(text) as Partial<ReloadRequest>
    if (!Array.isArray(value.argv) || !value.argv.every((arg) => typeof arg === "string")) return undefined
    return { argv: value.argv, ...(isDraft(value.draft) ? { draft: value.draft } : {}) }
  } catch {
    return undefined
  }
}

/** The supervisor's reload file and pid, removed from `env` (a TUI started from this app's shell is not supervised by it). */
export function takeSupervision(env: Record<string, string | undefined>): Supervision | undefined {
  const file = env[reloadFileEnv]
  const parent = Number(env[supervisorEnv])
  delete env[reloadFileEnv]
  delete env[supervisorEnv]
  if (!file || !Number.isInteger(parent) || parent <= 0) return undefined
  return { file, parent }
}

/** The state a reload handed over, removed from `env`; undefined for a first start. */
export function takeReloadState(env: Record<string, string | undefined>): ReloadState | undefined {
  const raw = env[reloadStateEnv]
  delete env[reloadStateEnv]
  if (raw === undefined) return undefined
  try {
    const value = JSON.parse(raw) as ReloadState
    if (typeof value !== "object" || value === null) return undefined
    return isDraft(value.draft) ? { draft: value.draft } : {}
  } catch {
    return undefined
  }
}

function isDraft(value: unknown): value is Draft {
  const draft = value as Draft | undefined
  return typeof draft === "object" && draft !== null && typeof draft.text === "string" && typeof draft.cursor === "number"
}
