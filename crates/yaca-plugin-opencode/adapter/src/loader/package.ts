import { readFile, stat } from "node:fs/promises"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { z } from "zod"

const PackageJsonSchema = z
  .object({
    exports: z.unknown().optional(),
    main: z.string().optional(),
    name: z.string().optional(),
  })
  .passthrough()

type PackageJson = Readonly<z.infer<typeof PackageJsonSchema>>

type PluginPackage = {
  readonly dir: string
  readonly json: PackageJson
}

export class NpmPluginPackageError extends Error {
  readonly name = "NpmPluginPackageError"
}

export async function resolveNpmPluginImportSpec(
  spec: string,
  configFilepath?: string,
): Promise<string> {
  const packageName = npmPackageNameFromSpec(spec)
  if (packageName === undefined) {
    return spec
  }
  const base =
    configFilepath === undefined ? process.cwd() : path.dirname(configFilepath)
  const pkg = await findPackage(base, packageName)
  if (pkg === undefined) {
    return spec
  }
  const entry = serverEntrypoint(spec, pkg)
  if (entry === undefined) {
    throw new NpmPluginPackageError(
      `npm plugin ${spec} does not expose a server entrypoint`,
    )
  }
  return entry
}

async function findPackage(
  start: string,
  packageName: string,
): Promise<PluginPackage | undefined> {
  let dir = path.resolve(start)
  while (true) {
    const candidate = path.join(dir, "node_modules", packageName, "package.json")
    const pkg = await readPackage(candidate)
    if (pkg !== undefined) {
      return pkg
    }
    const parent = path.dirname(dir)
    if (parent === dir) {
      return undefined
    }
    dir = parent
  }
}

async function readPackage(file: string): Promise<PluginPackage | undefined> {
  const info = await stat(file).catch((error: unknown) => {
    if (error instanceof Error) {
      return undefined
    }
    throw error
  })
  if (info === undefined || !info.isFile()) {
    return undefined
  }
  const raw = await readFile(file, "utf8")
  const parsed: unknown = JSON.parse(raw)
  return {
    dir: path.dirname(file),
    json: PackageJsonSchema.parse(parsed),
  }
}

function serverEntrypoint(spec: string, pkg: PluginPackage): string | undefined {
  const exports = pkg.json.exports
  if (isRecord(exports)) {
    const server = extractExportValue(exports["./server"])
    if (server !== undefined) {
      return packageFileUrl(spec, server, pkg)
    }
  }
  const main = packageMain(pkg.json)
  if (main !== undefined) {
    return packageFileUrl(spec, main, pkg)
  }
  if (isRecord(exports)) {
    return undefined
  }
  return pathToFileURL(pkg.dir).href
}

function packageMain(json: PackageJson): string | undefined {
  const value = json.main
  if (value === undefined) {
    return undefined
  }
  const trimmed = value.trim()
  return trimmed.length === 0 ? undefined : trimmed
}

function packageFileUrl(spec: string, raw: string, pkg: PluginPackage): string {
  const resolved = resolveExportPath(raw, pkg.dir)
  const root = path.resolve(pkg.dir)
  const candidate = path.resolve(resolved)
  if (!contains(root, candidate)) {
    throw new NpmPluginPackageError(
      `npm plugin ${spec} resolved server entry outside plugin directory`,
    )
  }
  return pathToFileURL(candidate).href
}

function resolveExportPath(raw: string, dir: string): string {
  if (raw.startsWith("file://")) {
    return fileURLToPath(raw)
  }
  if (path.isAbsolute(raw)) {
    return raw
  }
  return path.resolve(dir, raw)
}

function contains(root: string, candidate: string): boolean {
  const relative = path.relative(root, candidate)
  return (
    relative === "" ||
    (!relative.startsWith("..") && !path.isAbsolute(relative))
  )
}

function extractExportValue(value: unknown): string | undefined {
  if (typeof value === "string") {
    return value
  }
  if (!isRecord(value)) {
    return undefined
  }
  for (const key of ["import", "default"] as const) {
    const nested = value[key]
    if (typeof nested === "string") {
      return nested
    }
  }
  return undefined
}

export function npmPackageNameFromSpec(spec: string): string | undefined {
  const trimmed = spec.trim()
  if (trimmed.length === 0) {
    return undefined
  }
  if (trimmed.startsWith("@")) {
    return scopedPackageName(trimmed)
  }
  const first = firstPathSegment(trimmed)
  const version = first.indexOf("@")
  return version === -1 ? first : first.slice(0, version)
}

function scopedPackageName(spec: string): string | undefined {
  const firstSlash = spec.indexOf("/")
  if (firstSlash === -1) {
    return undefined
  }
  const secondSlash = spec.indexOf("/", firstSlash + 1)
  const first = secondSlash === -1 ? spec : spec.slice(0, secondSlash)
  const version = first.indexOf("@", 1)
  return version === -1 ? first : first.slice(0, version)
}

function firstPathSegment(spec: string): string {
  const slash = spec.indexOf("/")
  return slash === -1 ? spec : spec.slice(0, slash)
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
