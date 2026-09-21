/**
 * Parsing of Claude Code `marketplace.json` files (P7 v1).
 *
 * A marketplace manifest names a set of installable plugins. v1 supports
 * local-path sources only (relative to the marketplace root); git and
 * remote sources are surfaced as unsupported installable entries so the
 * installer can report them explicitly instead of silently skipping.
 */

import fs from "node:fs"
import path from "node:path"

import { MARKETPLACE_MANIFEST_FILE } from "./manifest_paths"

/** One installable plugin listed by a marketplace. */
export type MarketplaceEntry = {
  /** Plugin display name (the `name` field). */
  readonly name: string
  /** Local path relative to the marketplace root, when declared local. */
  readonly localPath?: string
  /** Set for git/remote sources; v1 cannot install these. */
  readonly unsupportedReason?: string
}

/** A parsed marketplace manifest rooted at a directory. */
export type Marketplace = {
  /** Directory containing `marketplace.json`. */
  readonly root: string
  /** Marketplace display name, when declared. */
  readonly name: string
  /** Installable entries in manifest order. */
  readonly plugins: readonly MarketplaceEntry[]
}

/** Why the directory is not a readable marketplace. */
export class MarketplaceError extends Error {}

/**
 * Read `<root>/marketplace.json` (also accepted:
 * `<root>/.claude-plugin/marketplace.json`).
 */
export function readMarketplace(root: string): Marketplace {
  const candidates = [
    path.join(root, MARKETPLACE_MANIFEST_FILE),
    path.join(root, ".claude-plugin", MARKETPLACE_MANIFEST_FILE),
  ]
  const manifestFile = candidates.find((candidate) => fs.existsSync(candidate))
  if (manifestFile === undefined) {
    throw new MarketplaceError(`${root} has no ${MARKETPLACE_MANIFEST_FILE}`)
  }
  let parsed: unknown
  try {
    parsed = JSON.parse(fs.readFileSync(manifestFile, "utf8"))
  } catch (error) {
    throw new MarketplaceError(`${MARKETPLACE_MANIFEST_FILE} is not valid JSON: ${String(error)}`)
  }
  if (!isRecord(parsed)) {
    throw new MarketplaceError(`${MARKETPLACE_MANIFEST_FILE} must contain an object`)
  }
  const rawPlugins = Array.isArray(parsed["plugins"]) ? parsed["plugins"] : []
  const plugins: MarketplaceEntry[] = []
  for (const raw of rawPlugins) {
    if (!isRecord(raw) || typeof raw["name"] !== "string" || raw["name"].length === 0) {
      continue
    }
    plugins.push(entryFrom(raw["name"], raw["source"]))
  }
  return {
    root,
    name: typeof parsed["name"] === "string" ? parsed["name"] : path.basename(root),
    plugins,
  }
}

/**
 * Resolve one marketplace entry to a plugin directory inside the marketplace
 * root. Local-path entries must stay within the root (path traversal is
 * rejected); unsupported entries throw with their reason.
 */
export function resolveMarketplaceEntry(
  marketplace: Marketplace,
  entry: MarketplaceEntry,
): string {
  if (entry.unsupportedReason !== undefined) {
    throw new MarketplaceError(
      `marketplace entry ${entry.name} is not installable: ${entry.unsupportedReason}`,
    )
  }
  const local = entry.localPath ?? entry.name
  const target = path.resolve(marketplace.root, local)
  const normalizedRoot = path.resolve(marketplace.root)
  if (target !== normalizedRoot && !target.startsWith(normalizedRoot + path.sep)) {
    throw new MarketplaceError(
      `marketplace entry ${entry.name} escapes the marketplace root`,
    )
  }
  return target
}

function entryFrom(name: string, source: unknown): MarketplaceEntry {
  if (source === undefined || source === null) {
    // Claude Code defaults to a same-named subdirectory of the marketplace.
    return { name, localPath: `./${name}` }
  }
  if (typeof source === "string") {
    if (source === name) {
      return { name, localPath: `./${name}` }
    }
    if (source.startsWith("./") || source.startsWith("../")) {
      // Relative path string; traversal is re-checked at resolve time.
      return { name, localPath: source }
    }
    return {
      name,
      unsupportedReason: `source ${JSON.stringify(source)} is not a local path (v1 installs local plugin directories only)`,
    }
  }
  if (isRecord(source) && typeof source["source"] === "string") {
    const kind = source["source"]
    if (kind === "local" || kind === "./") {
      const rawPath = source["path"]
      if (typeof rawPath === "string" && rawPath.length > 0) {
        return { name, localPath: rawPath }
      }
    }
    return {
      name,
      unsupportedReason: `source kind ${JSON.stringify(kind)} is not supported (v1 installs local plugin directories only)`,
    }
  }
  return {
    name,
    unsupportedReason: "source declaration is not a local path (v1 installs local plugin directories only)",
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
