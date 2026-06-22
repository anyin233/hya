import type { PluginOptions, PluginSpec } from "./discovery"
import {
  detectServerModuleShape,
  isPathPluginSpec,
  resolveLocalPluginSpec,
  type ServerPlugin,
} from "./shape"
import { isDeprecatedPluginSpec, resolveNpmPluginImportSpec } from "./package"

export type OpenCodeHooks = Readonly<Record<string, unknown>>

export type PluginLoadError = {
  readonly spec: string
  readonly message: string
}

export type LoadedPluginHooks = {
  readonly hooks: readonly OpenCodeHooks[]
  readonly errors: readonly PluginLoadError[]
}

export async function loadLocalPluginHooks(
  specs: readonly PluginSpec[],
  input: unknown,
  configFilepath?: string,
): Promise<LoadedPluginHooks> {
  const hooks: OpenCodeHooks[] = []
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
        errors.push({ spec: requested, message: errorMessage(caught) })
        return undefined
      },
    )
    if (spec === undefined) {
      continue
    }
    const loaded = await loadOnePlugin(spec, input, pluginOptions(plugin))
    hooks.push(...loaded.hooks)
    errors.push(...loaded.errors)
  }
  return { hooks, errors }
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
): Promise<LoadedPluginHooks> {
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
        return { hooks: [], errors: [] }
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
): Promise<LoadedPluginHooks> {
  const hooks: OpenCodeHooks[] = []
  for (const server of servers) {
    try {
      const result = await server(input, options)
      if (!isRecord(result)) {
        return error(spec, "plugin server did not return hooks object")
      }
      hooks.push(result)
    } catch (caught) {
      return error(spec, errorMessage(caught))
    }
  }
  return { hooks, errors: [] }
}

function pluginSpecifier(plugin: PluginSpec): string {
  return typeof plugin === "string" ? plugin : plugin[0]
}

function pluginOptions(plugin: PluginSpec): PluginOptions | undefined {
  return typeof plugin === "string" ? undefined : plugin[1]
}

function error(spec: string, message: string): LoadedPluginHooks {
  return { hooks: [], errors: [{ spec, message }] }
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
