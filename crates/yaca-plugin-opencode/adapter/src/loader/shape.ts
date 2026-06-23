import { stat } from "node:fs/promises"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

import type { PluginOptions, PluginSpec } from "./discovery"

const INDEX_FILES = [
  "index.ts",
  "index.tsx",
  "index.js",
  "index.mjs",
  "index.cjs",
] as const

export class PluginPathResolutionError extends Error {
  readonly name = "PluginPathResolutionError"

  constructor(readonly target: string) {
    super(`plugin directory ${target} is missing package.json or index file`)
  }
}

export type ServerPlugin = (
  input: unknown,
  options?: PluginOptions,
) => unknown | Promise<unknown>

export type ServerModuleShape =
  | {
      readonly kind: "v1_server"
      readonly id: string | undefined
      readonly server: ServerPlugin
    }
  | { readonly kind: "legacy_server"; readonly servers: readonly ServerPlugin[] }
  | { readonly kind: "tui_only" }
  | { readonly kind: "error"; readonly message: string }

export async function resolveLocalPluginSpec(
  plugin: PluginSpec,
  configFilepath: string,
): Promise<PluginSpec> {
  const specifier = pluginSpecifier(plugin)
  if (!isPathPluginSpec(specifier)) {
    return plugin
  }
  const target = pathLikeSpecToFileUrl(specifier, path.dirname(configFilepath))
  const resolved = await resolvePathPluginTarget(target).catch((error: unknown) => {
    if (error instanceof Error) {
      return target
    }
    throw error
  })
  return withSpecifier(plugin, resolved)
}

export async function resolvePathPluginTarget(spec: string): Promise<string> {
  const raw = spec.startsWith("file://") ? fileURLToPath(spec) : spec
  const file = isAbsolutePath(raw) ? raw : path.resolve(raw)
  const info = await stat(file).catch((error: unknown) => {
    if (error instanceof Error) {
      return undefined
    }
    throw error
  })
  if (info === undefined || !info.isDirectory()) {
    return pathToFileURL(file).href
  }
  const packageJson = await stat(path.join(file, "package.json")).catch(
    (error: unknown) => {
      if (error instanceof Error) {
        return undefined
      }
      throw error
    },
  )
  if (packageJson !== undefined) {
    return pathToFileURL(file).href
  }
  const index = await resolveDirectoryIndex(file)
  if (index !== undefined) {
    return pathToFileURL(index).href
  }
  throw new PluginPathResolutionError(file)
}

export function isPathPluginSpec(spec: string): boolean {
  return spec.startsWith("file://") || spec.startsWith(".") || isAbsolutePath(spec)
}

export function detectServerModuleShape(
  mod: Readonly<Record<string, unknown>>,
): ServerModuleShape {
  const v1 = detectV1ServerShape(mod)
  if (v1 !== undefined) {
    return v1
  }
  return detectLegacyServerShape(mod)
}

async function resolveDirectoryIndex(dir: string): Promise<string | undefined> {
  for (const name of INDEX_FILES) {
    const file = path.join(dir, name)
    const info = await stat(file).catch((error: unknown) => {
      if (error instanceof Error) {
        return undefined
      }
      throw error
    })
    if (info !== undefined && info.isFile()) {
      return file
    }
  }
  return undefined
}

function detectV1ServerShape(
  mod: Readonly<Record<string, unknown>>,
): ServerModuleShape | undefined {
  const value = mod.default
  if (!isRecord(value)) {
    return undefined
  }
  const hasV1Key = "id" in value || "server" in value || "tui" in value
  if (!hasV1Key) {
    return undefined
  }
  const server = value.server
  const tui = value.tui
  if (server !== undefined && !isServerPlugin(server)) {
    return { kind: "error", message: "invalid server export" }
  }
  if (tui !== undefined && typeof tui !== "function") {
    return { kind: "error", message: "invalid tui export" }
  }
  if (server !== undefined && tui !== undefined) {
    return { kind: "error", message: "mixed server and tui exports" }
  }
  if (tui !== undefined) {
    return { kind: "tui_only" }
  }
  if (server === undefined) {
    return { kind: "error", message: "missing server export" }
  }
  return {
    kind: "v1_server",
    id: typeof value.id === "string" ? value.id : undefined,
    server,
  }
}

function detectLegacyServerShape(
  mod: Readonly<Record<string, unknown>>,
): ServerModuleShape {
  const seen = new Set<unknown>()
  const servers: ServerPlugin[] = []
  for (const entry of Object.values(mod)) {
    if (seen.has(entry)) {
      continue
    }
    seen.add(entry)
    const server = legacyServer(entry)
    if (server === undefined) {
      return { kind: "error", message: "plugin export is not a function" }
    }
    servers.push(server)
  }
  if (servers.length === 0) {
    return { kind: "error", message: "plugin module has no exports" }
  }
  return { kind: "legacy_server", servers }
}

function legacyServer(value: unknown): ServerPlugin | undefined {
  if (isServerPlugin(value)) {
    return value
  }
  if (!isRecord(value)) {
    return undefined
  }
  const server = value.server
  return isServerPlugin(server) ? server : undefined
}

function pluginSpecifier(plugin: PluginSpec): string {
  return typeof plugin === "string" ? plugin : plugin[0]
}

function withSpecifier(plugin: PluginSpec, specifier: string): PluginSpec {
  if (typeof plugin === "string") {
    return specifier
  }
  return [specifier, plugin[1]]
}

function pathLikeSpecToFileUrl(spec: string, base: string): string {
  if (spec.startsWith("file://")) {
    return spec
  }
  if (isAbsolutePath(spec)) {
    return pathToFileURL(spec).href
  }
  return pathToFileURL(path.resolve(base, spec)).href
}

function isAbsolutePath(raw: string): boolean {
  return path.isAbsolute(raw) || /^[A-Za-z]:[\\/]/.test(raw)
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isServerPlugin(value: unknown): value is ServerPlugin {
  return typeof value === "function"
}
