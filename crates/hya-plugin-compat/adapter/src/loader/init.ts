import type { PluginOptions, PluginSpec } from "./discovery"
import {
  detectServerModuleShape,
  isPathPluginSpec,
  resolveLocalPluginSpec,
  type ServerPlugin,
} from "./shape"
import { isDeprecatedPluginSpec, resolveNpmPluginImportSpec } from "./package"
import {
  MAX_SKILL_CONTENT_BYTES,
  MAX_SKILL_DIGEST_BYTES,
  MAX_SKILL_ID_BYTES,
  type SkillContribution,
} from "../contributions"


export type CompatHooks = Readonly<Record<string, unknown>>

export type PluginLoadError = {
  readonly spec: string
  readonly message: string
  readonly kind: "load" | "declaration"
}

export type LoadedPluginContributions = {
  readonly hooks: readonly CompatHooks[]
  readonly skills: readonly SkillContribution[]
  readonly errors: readonly PluginLoadError[]
}


/** Typed failure raised when a loaded Compat plugin returns an invalid declaration. */
export class PluginDeclarationError extends Error {
  /** Stable class name used by callers when classifying declaration failures. */
  readonly name = "PluginDeclarationError"

  /** Create a declaration failure with contextual detail. */
  constructor(message: string) {
    super(message)
  }
}


export async function loadLocalPluginContributions(
  specs: readonly PluginSpec[],
  input: unknown,
  configFilepath?: string,
): Promise<LoadedPluginContributions> {
  const hooks: CompatHooks[] = []
  const skills: SkillContribution[] = []
  const skillIds = new Set<string>()
  const errors: PluginLoadError[] = []
  for (const original of specs) {
    if (isDeprecatedPluginSpec(pluginSpecifier(original))) {
      continue
    }
    const plugin =
      configFilepath === undefined
        ? original
        : await resolveLocalPluginSpec(original, configFilepath)
    const requested = pluginSpecifier(plugin)
    const spec = await resolvePluginImportSpec(requested, configFilepath).catch(
      (caught: unknown) => {
        errors.push({ spec: requested, message: errorMessage(caught), kind: "load" })
        return undefined
      },
    )
    if (spec === undefined) {
      continue
    }
    const loaded = await loadOnePlugin(spec, input, pluginOptions(plugin))
    hooks.push(...loaded.hooks)
    for (const skill of loaded.skills) {
      if (skillIds.has(skill.id)) {
        errors.push({
          spec: requested,
          message: `duplicate Skill contribution id: ${skill.id}`,
          kind: "declaration",
        })
        continue
      }
      skillIds.add(skill.id)
      skills.push(skill)
    }
    errors.push(...loaded.errors)
  }
  return { hooks, skills, errors }
}

async function resolvePluginImportSpec(
  spec: string,
  configFilepath: string | undefined,
): Promise<string> {
  if (isPathPluginSpec(spec)) {
    return spec
  }
  return resolveNpmPluginImportSpec(spec, configFilepath)
}

async function loadOnePlugin(
  spec: string,
  input: unknown,
  options: PluginOptions | undefined,
): Promise<LoadedPluginContributions> {
  try {
    const imported: unknown = await import(spec)
    if (!isRecord(imported)) {
      return error(spec, "plugin module is not an object")
    }
    const shape = detectServerModuleShape(imported)
    switch (shape.kind) {
      case "v1_server":
        return initServers(spec, [shape.server], input, options)
      case "legacy_server":
        return initServers(spec, shape.servers, input, options)
      case "tui_only":
        return { hooks: [], skills: [], errors: [] }
      case "error":
        return error(spec, shape.message)
    }
  } catch (caught) {
    return error(spec, errorMessage(caught))
  }
}


async function initServers(
  spec: string,
  servers: readonly ServerPlugin[],
  input: unknown,
  options: PluginOptions | undefined,
): Promise<LoadedPluginContributions> {
  const hooks: CompatHooks[] = []
  const skills: SkillContribution[] = []
  const skillIds = new Set<string>()
  for (const server of servers) {
    try {
      const result = await server(input, options)
      if (!isRecord(result)) {
        return error(spec, "plugin server did not return hooks object")
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
      hooks.push(result)
    } catch (caught) {
      return error(
        spec,
        errorMessage(caught),
        caught instanceof PluginDeclarationError ? "declaration" : "load",
      )
    }
  }
  return { hooks, skills, errors: [] }
}

function pluginSpecifier(plugin: PluginSpec): string {
  return typeof plugin === "string" ? plugin : plugin[0]
}

function pluginOptions(plugin: PluginSpec): PluginOptions | undefined {
  return typeof plugin === "string" ? undefined : plugin[1]
}

function error(
  spec: string,
  message: string,
  kind: PluginLoadError["kind"] = "load",
): LoadedPluginContributions {
  return { hooks: [], skills: [], errors: [{ spec, message, kind }] }
}

type SkillParseResult =
  | { readonly ok: true; readonly skills: readonly SkillContribution[] }
  | { readonly ok: false; readonly message: string }

/** Parse, bound, and verify optional Skill declarations from one Compat hook object. */
async function skillContributionsFrom(hook: CompatHooks): Promise<SkillParseResult> {
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

/** Encode UTF-8 text as a lowercase SHA-256 hexadecimal digest. */
async function sha256Hex(value: string): Promise<string> {
  const bytes = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))
  return Array.from(new Uint8Array(bytes), (byte) => byte.toString(16).padStart(2, "0")).join("")
}

/** Check non-empty UTF-8 text against one contribution byte bound. */
function boundedText(value: string, maxBytes: number): boolean {
  return value.length > 0 && new TextEncoder().encode(value).byteLength <= maxBytes
}


function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message
  }
  return String(error)
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
