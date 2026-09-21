import { stat } from "node:fs/promises"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

const INDEX_FILES = [
  "index.ts",
  "index.tsx",
  "index.js",
  "index.mjs",
  "index.cjs",
] as const

export class ExtensionPathResolutionError extends Error {
  readonly name = "ExtensionPathResolutionError"

  constructor(readonly target: string) {
    super(`extension directory ${target} is missing package.json or index file`)
  }
}

/**
 * An extension entry factory. It is always called with one frozen empty object
 * argument; extensions read host configuration from `process.env`.
 */
export type ExtensionFactory = (context: ExtensionContextInput) => unknown | Promise<unknown>

export type ExtensionContextInput = {
  readonly configDir?: string | undefined
  readonly env?: Readonly<Record<string, string | undefined>>
}

export type ExtensionModuleShape =
  | {
      readonly kind: "v1_server"
      readonly id: string | undefined
      readonly server: ExtensionFactory
    }
  | { readonly kind: "legacy_server"; readonly servers: readonly ExtensionFactory[] }
  | { readonly kind: "tui_only" }
  | { readonly kind: "error"; readonly message: string }

/**
 * Resolve an extension target (absolute path or file:// URL) into an
 * importable file:// specifier. Directories resolve to their package.json or
 * an index file.
 */
export async function resolveExtensionTarget(spec: string): Promise<string> {
  const raw = spec.startsWith("file://") ? fileURLToPath(spec) : spec
  const file = path.isAbsolute(raw) ? raw : path.resolve(raw)
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
  throw new ExtensionPathResolutionError(file)
}

export function detectServerModuleShape(
  mod: Readonly<Record<string, unknown>>,
): ExtensionModuleShape {
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
): ExtensionModuleShape | undefined {
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
  if (server !== undefined && !isExtensionFactory(server)) {
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
): ExtensionModuleShape {
  const seen = new Set<unknown>()
  const servers: ExtensionFactory[] = []
  for (const entry of Object.values(mod)) {
    if (seen.has(entry)) {
      continue
    }
    seen.add(entry)
    const server = legacyServer(entry)
    if (server === undefined) {
      return { kind: "error", message: "extension export is not a function" }
    }
    servers.push(server)
  }
  if (servers.length === 0) {
    return { kind: "error", message: "extension module has no exports" }
  }
  return { kind: "legacy_server", servers }
}

function legacyServer(value: unknown): ExtensionFactory | undefined {
  if (isExtensionFactory(value)) {
    return value
  }
  if (!isRecord(value)) {
    return undefined
  }
  const server = value.server
  return isExtensionFactory(server) ? server : undefined
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isExtensionFactory(value: unknown): value is ExtensionFactory {
  return typeof value === "function"
}
