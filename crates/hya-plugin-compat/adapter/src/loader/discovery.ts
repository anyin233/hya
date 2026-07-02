import { readFile, readdir, stat } from "node:fs/promises"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { parse as parseJsonc } from "jsonc-parser"
import type { ParseError as JsoncParseError } from "jsonc-parser"
import { z } from "zod"

import {
  globalConfigDir,
  compatConfigDirs,
  projectConfigFiles,
} from "./config_dirs"
import { npmPackageNameFromSpec } from "./package"
import { resolveLocalPluginSpec } from "./shape"

export class AdapterOptionsParseError extends Error {
  readonly name = "AdapterOptionsParseError"

  constructor(readonly detail: string) {
    super(`invalid Compat adapter options: ${detail}`)
  }
}

const PluginOptionsSchema = z.record(z.string(), z.unknown())
const PluginSpecSchema = z.union([
  z.string(),
  z.tuple([z.string(), PluginOptionsSchema]),
])
const AdapterOptionsSchema = z
  .object({
    plugin: z.array(PluginSpecSchema).optional().default([]),
  })
  .passthrough()
const GLOBAL_CONFIG_FILES = ["config.json", "opencode.json", "opencode.jsonc"] as const
const COMPAT_DIR_CONFIG_FILES = ["opencode.json", "opencode.jsonc"] as const

type ParsedPluginSpec = z.infer<typeof PluginSpecSchema>

export type PluginOptions = Readonly<Record<string, unknown>>
export type PluginSpec = string | readonly [string, PluginOptions]
export type AdapterOptions = {
  readonly plugin: readonly PluginSpec[]
}

export type DiscoveryContext = {
  readonly directory: string
  readonly worktree?: string
  readonly customConfigFile?: string
  readonly customConfigDir?: string
  readonly disableProjectConfig?: boolean
  readonly inlineConfig?: string
  readonly xdgConfigHome?: string
  readonly home?: string
}

export function parseAdapterOptions(raw: string | undefined): AdapterOptions {
  if (raw === undefined || raw.trim().length === 0) {
    return { plugin: [] }
  }
  let value: unknown
  try {
    value = JSON.parse(raw)
  } catch (error) {
    if (error instanceof SyntaxError) {
      throw new AdapterOptionsParseError(error.message)
    }
    if (error instanceof Error) {
      throw new AdapterOptionsParseError(error.message)
    }
    throw error
  }
  return parseAdapterValue(value)
}

function parseAdapterValue(value: unknown): AdapterOptions {
  const parsed = AdapterOptionsSchema.safeParse(value)
  if (!parsed.success) {
    throw new AdapterOptionsParseError(parsed.error.message)
  }
  return { plugin: parsed.data.plugin.map(normalizePluginSpec) }
}

export async function discoverPluginSpecs(
  context: DiscoveryContext,
): Promise<readonly PluginSpec[]> {
  const specs: PluginSpec[] = []
  const dirs = compatConfigDirs(context)
  const global = globalConfigDir(context)
  const [first, ...rest] = dirs
  const configDirs = first !== undefined && first === global ? rest : dirs
  if (first !== undefined && first === global) {
    specs.push(...(await readConfigDirPluginSpecs(first, GLOBAL_CONFIG_FILES)))
  }
  if (context.customConfigFile !== undefined && context.customConfigFile.length > 0) {
    specs.push(...(await readConfigFilePluginSpecs(context.customConfigFile)))
  }
  for (const file of projectConfigFiles(context)) {
    specs.push(...(await readConfigFilePluginSpecs(file)))
  }
  for (const dir of configDirs) {
    specs.push(...(await readConfigDirPluginSpecs(dir, COMPAT_DIR_CONFIG_FILES)))
  }
  if (context.inlineConfig !== undefined && context.inlineConfig.length > 0) {
    specs.push(...(await readInlineConfigPluginSpecs(context.inlineConfig, context.directory)))
  }
  return deduplicatePluginSpecs(specs)
}

async function scanPluginDir(dir: string): Promise<readonly string[]> {
  const dirStat = await stat(dir).catch((error: unknown) => {
    if (error instanceof Error) {
      return undefined
    }
    throw error
  })
  if (dirStat === undefined || !dirStat.isDirectory()) {
    return []
  }
  const entries = await readdir(dir, { withFileTypes: true }).catch(
    (error: unknown) => {
      if (error instanceof Error) {
        return []
      }
      throw error
    },
  )
  return entries
    .filter((entry) => entry.isFile() || entry.isSymbolicLink())
    .map((entry) => entry.name)
    .filter(isPluginFilename)
    .sort()
    .map((name) => pathToFileURL(path.join(dir, name)).href)
}

async function readConfigDirPluginSpecs(
  dir: string,
  names: readonly string[],
): Promise<readonly PluginSpec[]> {
  const specs: PluginSpec[] = []
  for (const name of names) {
    specs.push(...(await readConfigFilePluginSpecs(path.join(dir, name))))
  }
  for (const child of ["plugin", "plugins"] as const) {
    specs.push(...(await scanPluginDir(path.join(dir, child))))
  }
  return specs
}

async function readConfigFilePluginSpecs(
  file: string,
): Promise<readonly PluginSpec[]> {
  const raw = await readConfigFile(file)
  if (raw === undefined) {
    return []
  }
  const options = parseConfigOptions(raw)
  const specs: PluginSpec[] = []
  for (const plugin of options.plugin) {
    specs.push(await resolveLocalPluginSpec(plugin, file))
  }
  return specs
}

async function readInlineConfigPluginSpecs(
  raw: string,
  directory: string,
): Promise<readonly PluginSpec[]> {
  const options = parseConfigOptions(raw)
  const virtualFile = path.join(directory, "COMPAT_CONFIG_CONTENT")
  const specs: PluginSpec[] = []
  for (const plugin of options.plugin) {
    specs.push(await resolveLocalPluginSpec(plugin, virtualFile))
  }
  return specs
}

async function readConfigFile(file: string): Promise<string | undefined> {
  return readFile(file, "utf8").catch((error: unknown) => {
    if (error instanceof Error) {
      return undefined
    }
    throw error
  })
}

function parseConfigOptions(raw: string): AdapterOptions {
  try {
    const errors: JsoncParseError[] = []
    const value: unknown = parseJsonc(raw, errors, {
      allowTrailingComma: true,
    })
    if (errors.length > 0) {
      return { plugin: [] }
    }
    return parseAdapterValue(value)
  } catch (error) {
    if (error instanceof AdapterOptionsParseError) {
      return { plugin: [] }
    }
    throw error
  }
}

function deduplicatePluginSpecs(specs: readonly PluginSpec[]): readonly PluginSpec[] {
  const seen = new Set<string>()
  const keep: PluginSpec[] = []
  for (const spec of specs.toReversed()) {
    const identity = pluginIdentity(spec)
    if (seen.has(identity)) {
      continue
    }
    seen.add(identity)
    keep.push(spec)
  }
  return keep.toReversed()
}

function pluginIdentity(plugin: PluginSpec): string {
  const spec = pluginSpecifier(plugin)
  return spec.startsWith("file://") ? spec : (npmPackageNameFromSpec(spec) ?? spec)
}

function pluginSpecifier(plugin: PluginSpec): string {
  return typeof plugin === "string" ? plugin : plugin[0]
}

function normalizePluginSpec(spec: ParsedPluginSpec): PluginSpec {
  if (typeof spec === "string") {
    return spec
  }
  const [specifier, options] = spec
  return [specifier, Object.freeze({ ...options })]
}

function isPluginFilename(name: string): boolean {
  const extension = path.extname(name).toLowerCase()
  return extension === ".js" || extension === ".ts"
}
