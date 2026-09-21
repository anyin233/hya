import {
  MAX_SKILL_CONTENT_BYTES,
  MAX_SKILL_DIGEST_BYTES,
  MAX_SKILL_ID_BYTES,
  type SkillContribution,
  type WorkspaceAdapterContribution,
} from "../contributions"
import { isRecord } from "../validate"
import {
  detectServerModuleShape,
  resolveExtensionTarget,
  type ExtensionContextInput,
  type ExtensionFactory,
} from "./shape"

/**
 * One loaded extension contribution object: the record returned by the
 * extension's server factory. Hooks are registered as functions under the
 * exact hya wire names.
 */
export type ExtensionHooks = Readonly<Record<string, unknown>>

export type ExtensionLoadError = {
  readonly spec: string
  readonly message: string
  readonly kind: "load" | "declaration"
}

export type LoadedExtensionContributions = {
  readonly hooks: readonly ExtensionHooks[]
  readonly skills: readonly SkillContribution[]
  readonly workspaceAdapters: readonly WorkspaceAdapterContribution[]
  readonly errors: readonly ExtensionLoadError[]
}

/** Typed failure raised when a loaded extension returns an invalid declaration. */
export class ExtensionDeclarationError extends Error {
  /** Stable class name used by callers when classifying declaration failures. */
  readonly name = "ExtensionDeclarationError"

  /** Create a declaration failure with contextual detail. */
  constructor(message: string) {
    super(message)
  }
}

/**
 * Load every extension path and collect its declared contributions. Each
 * module's server factory is awaited with one frozen empty object argument.
 */
export async function loadExtensionContributions(
  specs: readonly string[],
  env: Readonly<Record<string, string | undefined>>,
): Promise<LoadedExtensionContributions> {
  const context = extensionContextInput(env)
  const hooks: ExtensionHooks[] = []
  const skills: SkillContribution[] = []
  const skillIds = new Set<string>()
  const workspaceAdapters: WorkspaceAdapterContribution[] = []
  const workspaceKeys = new Set<string>()
  const errors: ExtensionLoadError[] = []
  for (const original of specs) {
    const spec = await resolveExtensionTarget(original).catch((caught: unknown) => {
      errors.push({ spec: original, message: errorMessage(caught), kind: "load" })
      return undefined
    })
    if (spec === undefined) {
      continue
    }
    const loaded = await loadOneExtension(spec, context)
    hooks.push(...loaded.hooks)
    for (const skill of loaded.skills) {
      if (skillIds.has(skill.id)) {
        errors.push({
          spec: original,
          message: `duplicate Skill contribution id: ${skill.id}`,
          kind: "declaration",
        })
        continue
      }
      skillIds.add(skill.id)
      skills.push(skill)
    }
    for (const adapter of loaded.workspaceAdapters) {
      const key = `${adapter.type}:${adapter.name}`
      if (workspaceKeys.has(key)) {
        errors.push({
          spec: original,
          message: `duplicate workspace adapter declaration: ${key}`,
          kind: "declaration",
        })
        continue
      }
      workspaceKeys.add(key)
      workspaceAdapters.push(adapter)
    }
    errors.push(...loaded.errors)
  }
  return { hooks, skills, workspaceAdapters, errors }
}

/**
 * Build the factory context. Every extension factory receives one frozen empty
 * object, matching the long-standing bundle activation contract: extensions
 * read host configuration from `process.env` (e.g. `HYA_BUNDLE_CONFIG_DIR`),
 * not from the factory argument.
 */
function extensionContextInput(
  _env: Readonly<Record<string, string | undefined>>,
): ExtensionContextInput {
  return Object.freeze({})
}

async function loadOneExtension(
  spec: string,
  context: ExtensionContextInput,
): Promise<LoadedExtensionContributions> {
  try {
    const imported: unknown = await import(spec)
    if (!isRecord(imported)) {
      return error(spec, "extension module is not an object")
    }
    const shape = detectServerModuleShape(imported)
    switch (shape.kind) {
      case "v1_server":
        return initServers(spec, [shape.server], context)
      case "legacy_server":
        return initServers(spec, shape.servers, context)
      case "tui_only":
        return empty()
      case "error":
        return error(spec, shape.message)
    }
  } catch (caught) {
    return error(spec, errorMessage(caught))
  }
}

async function initServers(
  spec: string,
  servers: readonly ExtensionFactory[],
  context: ExtensionContextInput,
): Promise<LoadedExtensionContributions> {
  const hooks: ExtensionHooks[] = []
  const skills: SkillContribution[] = []
  const workspaceAdapters: WorkspaceAdapterContribution[] = []
  const skillIds = new Set<string>()
  for (const server of servers) {
    try {
      const result = await server(context)
      if (!isRecord(result)) {
        return error(spec, "extension server did not return a contribution object")
      }
      const parsedSkills = await skillContributionsFrom(result)
      if (!parsedSkills.ok) {
        return error(spec, parsedSkills.message, "declaration")
      }
      for (const skill of parsedSkills.skills) {
        if (skillIds.has(skill.id)) {
          return error(
            spec,
            `duplicate Skill contribution id: ${skill.id}`,
            "declaration",
          )
        }
        skillIds.add(skill.id)
        skills.push(skill)
      }
      const parsedAdapters = workspaceAdapterContributionsFrom(result)
      if (!parsedAdapters.ok) {
        return error(spec, parsedAdapters.message, "declaration")
      }
      workspaceAdapters.push(...parsedAdapters.adapters)
      hooks.push(result)
    } catch (caught) {
      return error(
        spec,
        errorMessage(caught),
        caught instanceof ExtensionDeclarationError ? "declaration" : "load",
      )
    }
  }
  return { hooks, skills, workspaceAdapters, errors: [] }
}

function empty(): LoadedExtensionContributions {
  return { hooks: [], skills: [], workspaceAdapters: [], errors: [] }
}

function error(
  spec: string,
  message: string,
  kind: ExtensionLoadError["kind"] = "load",
): LoadedExtensionContributions {
  return { hooks: [], skills: [], workspaceAdapters: [], errors: [{ spec, message, kind }] }
}

type SkillParseResult =
  | { readonly ok: true; readonly skills: readonly SkillContribution[] }
  | { readonly ok: false; readonly message: string }

/** Parse, bound, and verify optional Skill declarations from one extension object. */
async function skillContributionsFrom(hook: ExtensionHooks): Promise<SkillParseResult> {
  const raw = hook.skills
  if (raw === undefined) {
    return { ok: true, skills: [] }
  }
  if (!Array.isArray(raw)) {
    return { ok: false, message: "skills must be an array" }
  }

  const skills: SkillContribution[] = []
  const seen = new Set<string>()
  for (const [index, value] of raw.entries()) {
    if (!isRecord(value)) {
      return { ok: false, message: `skills[${index}] must be an object` }
    }
    const unknown = Object.keys(value).find(
      (key) => key !== "id" && key !== "content" && key !== "digest",
    )
    if (unknown !== undefined) {
      return {
        ok: false,
        message: `skills[${index}] has unknown field: ${unknown}`,
      }
    }
    const id = value.id
    const content = value.content
    const digest = value.digest
    if (typeof id !== "string" || typeof content !== "string" || typeof digest !== "string") {
      return {
        ok: false,
        message: `skills[${index}] requires string id, content, and digest`,
      }
    }
    if (
      !boundedText(id, MAX_SKILL_ID_BYTES) ||
      !boundedText(content, MAX_SKILL_CONTENT_BYTES) ||
      !boundedText(digest, MAX_SKILL_DIGEST_BYTES)
    ) {
      return { ok: false, message: `skills[${index}] exceeds a contribution bound` }
    }
    if (!/^[0-9a-f]{64}$/.test(digest)) {
      return {
        ok: false,
        message: `skills[${index}] digest must be 64 lowercase SHA-256 hex characters`,
      }
    }
    const expected = await sha256Hex(content)
    if (digest !== expected) {
      return {
        ok: false,
        message: `skills[${index}] digest does not match UTF-8 content (expected ${expected})`,
      }
    }
    if (seen.has(id)) {
      return { ok: false, message: `duplicate Skill contribution id: ${id}` }
    }
    seen.add(id)
    skills.push({ id, content, digest })
  }
  return { ok: true, skills }
}

type WorkspaceAdapterParseResult =
  | { readonly ok: true; readonly adapters: readonly WorkspaceAdapterContribution[] }
  | { readonly ok: false; readonly message: string }

/** Parse optional workspace adapter declarations from one extension object. */
function workspaceAdapterContributionsFrom(
  hook: ExtensionHooks,
): WorkspaceAdapterParseResult {
  const raw = hook.workspaceAdapters
  if (raw === undefined) {
    return { ok: true, adapters: [] }
  }
  if (!Array.isArray(raw)) {
    return { ok: false, message: "workspaceAdapters must be an array" }
  }
  const adapters: WorkspaceAdapterContribution[] = []
  for (const [index, value] of raw.entries()) {
    if (!isRecord(value)) {
      return { ok: false, message: `workspaceAdapters[${index}] must be an object` }
    }
    const type = value.type
    const name = value.name
    const description = value.description
    if (
      typeof type !== "string" ||
      type.length === 0 ||
      typeof name !== "string" ||
      name.length === 0 ||
      typeof description !== "string"
    ) {
      return {
        ok: false,
        message: `workspaceAdapters[${index}] requires string type, name, and description`,
      }
    }
    adapters.push({ type, name, description })
  }
  return { ok: true, adapters }
}

/** Encode UTF-8 text as a lowercase SHA-256 hexadecimal digest. */
async function sha256Hex(value: string): Promise<string> {
  const bytes = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))
  return Array.from(new Uint8Array(bytes), (byte) => byte.toString(16).padStart(2, "0")).join("")
}

/** Check non-empty UTF-8 text against one contribution byte bound. */
function boundedText(value: string, maxBytes: number): boolean {
  return value.length > 0 && new TextEncoder().encode(value).byteLength <= maxBytes
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message
  }
  return String(error)
}
