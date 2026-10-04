/**
 * From a `ListTuiExtensions` catalog entry to runnable local files: validate
 * the descriptor, check every file's sha256, and write the files under
 * `<cache>/<prepared digest>/` (written once; a digest names exact content).
 * The extension process (packages/hya-tui-sdk/src/main.ts) bundles them with
 * the SDK, which lives next to the TUI: `lib/hya/tui-sdk` in a release,
 * `packages/hya-tui-sdk` in the repository.
 */
import { createHash } from "node:crypto"
import { existsSync, readFileSync } from "node:fs"
import { access, mkdir, rename, rm, writeFile } from "node:fs/promises"
import { dirname, isAbsolute, join, normalize, resolve, sep } from "node:path"
import { extensionApiVersion, extensionPermissions, extensionSdkMajor, type ExtensionPermission } from "./wire"

/** One verified catalog row (`TuiExtensionDescriptor`). */
export interface CatalogExtension {
  readonly bundleId: string
  readonly bundleVersion: string
  readonly preparedDigest: string
  readonly entry: string
  readonly sdk: string
  readonly permissions: readonly ExtensionPermission[]
  readonly files: readonly { readonly path: string; readonly sha256: string; readonly content: string }[]
  readonly firstParty: boolean
  /** The server left `files` empty: the TUI reported this digest as cached. */
  readonly cached: boolean
}

export interface InstalledSdk { readonly dir: string; readonly version: string }

const maxFiles = 256
const maxBytes = 8 * 1024 * 1024

/** A bundle-relative path that cannot leave the extension directory. */
function safeRelative(path: string): boolean {
  if (!path || isAbsolute(path) || path.includes("\\") || path.includes("\0")) return false
  const normal = normalize(path)
  return normal === path && !normal.startsWith("..") && !normal.split(sep).includes("..")
}

/** Validate an untrusted catalog row; a string says why it was rejected. */
export function parseCatalogEntry(value: unknown): CatalogExtension | string {
  if (!value || typeof value !== "object") return "catalog entry is not an object"
  const row = value as Record<string, unknown>
  const id = typeof row.bundleId === "string" ? row.bundleId : ""
  if (!/^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$/.test(id)) return `invalid bundle id ${JSON.stringify(row.bundleId)}`
  if ((row.apiVersion ?? 0) !== extensionApiVersion) return `${id}: api_version ${String(row.apiVersion)} is not supported (host speaks ${extensionApiVersion})`
  if (typeof row.preparedDigest !== "string" || !/^[0-9a-f]{64}$/.test(row.preparedDigest)) return `${id}: invalid prepared digest`
  if (typeof row.entry !== "string" || !safeRelative(row.entry)) return `${id}: invalid entry`
  if (typeof row.sdk !== "string") return `${id}: missing sdk version`
  const permissions = Array.isArray(row.permissions) ? row.permissions : []
  const unknown = permissions.find((permission) => !extensionPermissions.includes(permission as ExtensionPermission))
  if (unknown !== undefined) return `${id}: unknown permission ${String(unknown)}`
  const files = Array.isArray(row.files) ? row.files as Record<string, unknown>[] : []
  const cached = row.cached === true
  if (files.length === 0 && !cached) return `${id}: expected 1-${maxFiles} extension files`
  if (files.length > maxFiles) return `${id}: expected at most ${maxFiles} extension files`
  let total = 0
  for (const file of files) {
    if (!file || typeof file.path !== "string" || !safeRelative(file.path) || typeof file.content !== "string" || typeof file.sha256 !== "string") return `${id}: malformed extension file`
    if (createHash("sha256").update(file.content).digest("hex") !== file.sha256) return `${id}: ${file.path} does not match its sha256`
    total += file.content.length
  }
  if (total > maxBytes) return `${id}: extension files exceed ${maxBytes} bytes`
  if (!cached && !files.some((file) => file.path === row.entry)) return `${id}: entry ${row.entry} is not among its files`
  return {
    bundleId: id, bundleVersion: typeof row.bundleVersion === "string" ? row.bundleVersion : "", preparedDigest: row.preparedDigest,
    entry: row.entry, sdk: row.sdk, permissions: permissions as ExtensionPermission[],
    files: files as unknown as CatalogExtension["files"],
    firstParty: row.first_party === true || row.firstParty === true,
    cached,
  }
}

/** The SDK shipped next to this TUI, or `undefined` (then no extension can run). */
export function locateSdk(from = import.meta.dir): InstalledSdk | undefined {
  for (const name of ["tui-sdk", "hya-tui-sdk"]) {
    const dir = resolve(from, "../../..", name)
    try {
      const manifest = JSON.parse(readFileSync(join(dir, "package.json"), "utf8")) as { version?: unknown }
      if (typeof manifest.version === "string" && existsSync(join(dir, "src/main.ts"))) return { dir, version: manifest.version }
    } catch { /* try the next layout */ }
  }
  return undefined
}

/** `required` (`major.minor[.patch]`) runs on `installed` when the majors match the host's and the installed minor is not older. */
export function sdkCompatible(required: string, installed: string): boolean {
  const [major, minor = 0] = required.split(".").map(Number)
  const [hasMajor, hasMinor = 0] = installed.split(".").map(Number)
  return major === extensionSdkMajor && hasMajor === extensionSdkMajor && Number.isInteger(minor) && minor <= hasMinor
}

/** `$XDG_CACHE_HOME/hya/tui-extensions`, else `$HOME/.cache/hya/tui-extensions`. */
export function extensionCacheRoot(env: Record<string, string | undefined>): string {
  return join(env.XDG_CACHE_HOME || join(env.HOME ?? ".", ".cache"), "hya", "tui-extensions")
}

/** Write the extension's files (once per digest) and return their directory. */
export async function materialize(extension: CatalogExtension, cacheRoot: string): Promise<string> {
  const dir = join(cacheRoot, extension.preparedDigest)
  const ready = join(dir, ".complete")
  const present = await access(ready).then(() => true, () => false)
  // The catalog omits files the TUI reported as cached; a cache removed since then cannot be rebuilt here.
  if (!present && extension.cached) throw new Error(`cached files for ${extension.preparedDigest} are gone; reload the catalog`)
  if (!present) {
    const staging = `${dir}.tmp-${process.pid}-${Date.now()}`
    await rm(staging, { recursive: true, force: true })
    for (const file of extension.files) {
      const target = join(staging, file.path)
      await mkdir(dirname(target), { recursive: true })
      await writeFile(target, file.content, { mode: 0o444 })
    }
    await writeFile(join(staging, ".complete"), extension.preparedDigest)
    await rm(dir, { recursive: true, force: true })
    await mkdir(cacheRoot, { recursive: true })
    await rename(staging, dir)
  }
  return dir
}
