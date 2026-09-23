// hya-extra/model-fallback — a self-contained Bun script that speaks the hya
// plugin protocol v1 (newline-delimited JSON-RPC 2.0 over stdio) directly.
//
// Declared with `extensions.process: { kind: bun, command: [...] }`, so it
// does NOT get the `hya-plugin-bun` adapter injected (that only happens for
// implicit JavaScript Plugins) — this file owns the whole wire surface it
// needs: `initialize` and `hook/model.fallback`, plus `{}` replies to any
// other id-bearing method it is asked. See docs/plugin-protocol.md.
//
// Pure decision logic (`parseConfig`, `decide`) is exported for `bun test`
// and does not touch stdio; the process loop below is guarded by
// `import.meta.main` so importing this file for tests never starts it.

import { createInterface } from "node:readline"

const PLUGIN_ID = "model-fallback"
const PLUGIN_VERSION = "1.0.0"

/** The five provider failure classes the `model.fallback` hook can see. */
export type ErrorClass =
  | "retryable"
  | "unknown_model"
  | "auth"
  | "invalid_request"
  | "other"

export const ALL_ERROR_CLASSES: readonly ErrorClass[] = [
  "retryable",
  "unknown_model",
  "auth",
  "invalid_request",
  "other",
]

/** Validated `config.yml` contract for this bundle. */
export interface FallbackConfig {
  /** Per-model fallback chain, keyed by the ORIGINAL failing model of a round. */
  readonly chains: Readonly<Record<string, readonly string[]>>
  /** Fallback chain used for a model with no entry in `chains`. */
  readonly default: readonly string[]
  /** Error classes that trigger a fallback consult; `"any"` matches every class. */
  readonly on: readonly string[]
  /** Bundle-side cap on retries per round (clamped to the engine's 8-attempt cap). */
  readonly max_attempts: number
}

/** Engine hard cap on provider attempts per round (`docs/plugin-protocol.md`). */
const ENGINE_MAX_ATTEMPTS = 8

export const DEFAULT_CONFIG: FallbackConfig = Object.freeze({
  chains: Object.freeze({}),
  default: Object.freeze([]),
  on: Object.freeze(["retryable", "unknown_model"]),
  max_attempts: 3,
})

/** `hook/model.fallback` params (see docs/plugin-protocol.md). */
export interface FallbackParams {
  readonly session: string
  readonly root_session: string
  readonly agent?: string
  readonly message: string
  readonly model: string
  readonly error: { readonly class: string; readonly message: string }
  readonly attempt: number
  readonly tried: readonly string[]
}

export type FallbackOutcome =
  | { readonly outcome: "retry"; readonly model: string }
  | { readonly outcome: "give_up" }

const GIVE_UP: FallbackOutcome = { outcome: "give_up" }

/**
 * Parse and validate a `config.yml` body. Never throws: a missing file
 * (`text` undefined/empty), a YAML parse error, or a structurally invalid
 * document all fall back to [`DEFAULT_CONFIG`] (empty chains, so `decide`
 * naturally gives up on every consult) and log a diagnostic to stderr.
 */
export function parseConfig(text: string | undefined): FallbackConfig {
  if (text === undefined || text.trim().length === 0) {
    return DEFAULT_CONFIG
  }
  let raw: unknown
  try {
    raw = Bun.YAML.parse(text)
  } catch (error) {
    logError(`invalid model-fallback config.yml (YAML parse error): ${describe(error)}`)
    return DEFAULT_CONFIG
  }
  try {
    return validateConfig(raw)
  } catch (error) {
    logError(`invalid model-fallback config.yml: ${describe(error)}`)
    return DEFAULT_CONFIG
  }
}

function validateConfig(raw: unknown): FallbackConfig {
  if (!isRecord(raw)) {
    logError("model-fallback config.yml must be a YAML mapping; using defaults")
    return DEFAULT_CONFIG
  }
  const chains = validateChains(raw.chains)
  const parsedDefault = validateStringArray(raw.default)
  if (parsedDefault === undefined && raw.default !== undefined) {
    logError("model-fallback config.yml: `default` must be a list of strings; using defaults")
  }
  const fallbackDefault = parsedDefault ?? DEFAULT_CONFIG.default
  const on = validateOn(raw.on) ?? DEFAULT_CONFIG.on
  const maxAttempts = validateMaxAttempts(raw.max_attempts) ?? DEFAULT_CONFIG.max_attempts
  return {
    chains,
    default: fallbackDefault,
    on,
    max_attempts: maxAttempts,
  }
}

function validateChains(value: unknown): Readonly<Record<string, readonly string[]>> {
  if (!isRecord(value)) {
    if (value !== undefined) {
      logError("model-fallback config.yml: `chains` must be a mapping; ignoring")
    }
    return DEFAULT_CONFIG.chains
  }
  const chains: Record<string, readonly string[]> = {}
  for (const [model, candidates] of Object.entries(value)) {
    const parsed = validateStringArray(candidates)
    if (parsed === undefined) {
      logError(`model-fallback config.yml: chains.${model} must be a list of strings; ignoring entry`)
      continue
    }
    chains[model] = parsed
  }
  return chains
}

function validateStringArray(value: unknown): readonly string[] | undefined {
  if (!Array.isArray(value)) {
    return undefined
  }
  const result: string[] = []
  for (const entry of value) {
    if (typeof entry !== "string" || entry.trim().length === 0) {
      return undefined
    }
    result.push(entry)
  }
  return result
}

const KNOWN_ON_VALUES = new Set<string>([...ALL_ERROR_CLASSES, "any"])

function validateOn(value: unknown): readonly string[] | undefined {
  const parsed = validateStringArray(value)
  if (parsed === undefined) {
    if (value !== undefined) {
      logError("model-fallback config.yml: `on` must be a list of strings; using defaults")
    }
    return undefined
  }
  for (const entry of parsed) {
    if (!KNOWN_ON_VALUES.has(entry)) {
      logError(`model-fallback config.yml: \`on\` has unknown error class "${entry}"; using defaults`)
      return undefined
    }
  }
  if (parsed.length === 0) {
    logError("model-fallback config.yml: `on` must not be empty; using defaults")
    return undefined
  }
  return parsed
}

function validateMaxAttempts(value: unknown): number | undefined {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1) {
    if (value !== undefined) {
      logError("model-fallback config.yml: `max_attempts` must be a positive integer; using default")
    }
    return undefined
  }
  if (value > ENGINE_MAX_ATTEMPTS) {
    logError(
      `model-fallback config.yml: max_attempts ${value} exceeds the engine cap of ${ENGINE_MAX_ATTEMPTS}; clamping`,
    )
    return ENGINE_MAX_ATTEMPTS
  }
  return value
}

/**
 * Decide the next model for one `model.fallback` consult. Never throws: any
 * unexpected shape or exception reads as give-up.
 *
 * The chain walked is always the ORIGINAL failing model of the round
 * (`tried[0]`), not the model that just failed — so A -> B -> C walks A's
 * chain, never B's.
 */
export function decide(params: FallbackParams, config: FallbackConfig): FallbackOutcome {
  try {
    const classes = config.on.includes("any") ? ALL_ERROR_CLASSES : config.on
    if (!classes.includes(params.error.class as ErrorClass)) {
      return GIVE_UP
    }
    if (!Number.isFinite(params.attempt) || params.attempt > config.max_attempts) {
      return GIVE_UP
    }
    const tried = Array.isArray(params.tried) ? params.tried : []
    const original = tried.length > 0 ? tried[0] : params.model
    const chain = Object.prototype.hasOwnProperty.call(config.chains, original)
      ? config.chains[original]
      : config.default
    const next = chain.find((candidate) => !tried.includes(candidate))
    if (next === undefined) {
      return GIVE_UP
    }
    return { outcome: "retry", model: next }
  } catch (error) {
    logError(`model.fallback decide() failed: ${describe(error)}`)
    return GIVE_UP
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function logError(message: string): void {
  console.error(`[hya-extra/model-fallback] ${message}`)
}

// --- stdio JSON-RPC loop (skipped when this file is imported for tests) ---

async function readConfigFile(): Promise<string | undefined> {
  const file = process.env.HYA_BUNDLE_CONFIG_FILE
  if (!file) {
    return undefined
  }
  try {
    const bunFile = Bun.file(file)
    if (!(await bunFile.exists())) {
      return undefined
    }
    return await bunFile.text()
  } catch (error) {
    logError(`failed reading config.yml at ${file}: ${describe(error)}`)
    return undefined
  }
}

function reply(id: number, result: unknown): void {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`)
}

async function main(): Promise<void> {
  const config = parseConfig(await readConfigFile())
  const rl = createInterface({ input: process.stdin, terminal: false })
  for await (const rawLine of rl) {
    const line = rawLine.trim()
    if (line.length === 0) {
      continue
    }
    let request: unknown
    try {
      request = JSON.parse(line)
    } catch {
      // Malformed frame; there is no id to reply to.
      continue
    }
    if (!isRecord(request)) {
      continue
    }
    const id = request.id
    // Notifications (no id, e.g. `event`) never get a reply.
    if (typeof id !== "number") {
      continue
    }
    const method = request.method
    if (method === "initialize") {
      reply(id, {
        protocol_version: 1,
        plugin: { id: PLUGIN_ID, version: PLUGIN_VERSION, kind: "bun" },
        hooks: [{ name: "model.fallback", posture: "open" }],
        tools: [],
        skills: [],
      })
      continue
    }
    if (method === "hook/model.fallback") {
      try {
        const params = request.params as FallbackParams
        reply(id, decide(params, config))
      } catch (error) {
        logError(`hook/model.fallback request failed: ${describe(error)}`)
        reply(id, GIVE_UP)
      }
      continue
    }
    // Every other id-bearing method (including `shutdown`) gets an empty
    // reply; the host closes stdin afterward and this loop exits on EOF.
    reply(id, {})
  }
}

if (import.meta.main) {
  main().catch((error) => {
    logError(`fatal: ${describe(error)}`)
    process.exit(1)
  })
}
