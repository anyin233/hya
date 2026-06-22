import { readFile, readdir, stat } from "node:fs/promises"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { parse as parseJsonc } from "jsonc-parser"
import type { ParseError as JsoncParseError } from "jsonc-parser"
import { z } from "zod"

import { resolveLocalPluginSpec } from "./shape"

export class AdapterOptionsParseError extends Error {
  readonly name = "AdapterOptionsParseError"

  constructor(readonly detail: string) {
    super(`invalid OpenCode adapter options: ${detail}`)
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
const CONFIG_FILES = ["config.json", "opencode.json", "opencode.jsonc"] as const

type ParsedPluginSpec = z.infer<typeof PluginSpecSchema>

export type PluginOptions = Readonly<Record<string, unknown>>
export type PluginSpec = string | readonly [string, PluginOptions]
export type AdapterOptions = {
  readonly plugin: readonly PluginSpec[]
}

export type DiscoveryContext = {
  readonly directory: string
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
  for (const dir of opencodeConfigDirs(context)) {
    specs.push(...(await readConfigPluginSpecs(dir)))
    for (const child of ["plugin", "plugins"] as const) {
      specs.push(...(await scanPluginDir(path.join(dir, child))))
    }
  }
  return specs
}

export function opencodeConfigDirs(
  context: DiscoveryContext,
): readonly string[] {
  const dirs: string[] = []
  const global = globalConfigDir(context)
  if (global !== undefined) {
    dirs.push(global)
  }
  dirs.push(path.join(context.directory, ".opencode"))
  return [...new Set(dirs)]
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

async function readConfigPluginSpecs(dir: string): Promise<readonly PluginSpec[]> {
  const specs: PluginSpec[] = []
  for (const name of CONFIG_FILES) {
    const file = path.join(dir, name)
    const raw = await readConfigFile(file)
    if (raw === undefined) {
      continue
    }
    const options = parseConfigOptions(raw)
    for (const plugin of options.plugin) {
      specs.push(await resolveLocalPluginSpec(plugin, file))
    }
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

function globalConfigDir(context: DiscoveryContext): string | undefined {
  if (context.xdgConfigHome !== undefined && context.xdgConfigHome.length > 0) {
    return path.join(context.xdgConfigHome, "opencode")
  }
  const home = context.home ?? process.env.HOME
  if (home === undefined || home.length === 0) {
    return undefined
  }
  return path.join(home, ".config", "opencode")
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
