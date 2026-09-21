/**
 * Discovery of Claude Code plugin sources.
 *
 * Layouts understood (checked in order for one plugin directory):
 *
 * - `<dir>/plugin.json` — flat Claude Code plugin layout
 * - `<dir>/.claude-plugin/plugin.json` — metadata-subdirectory layout
 * - `<dir>/CLAUDE.md`-less legacy layout is not supported
 *
 * Environment-wide discovery scans the project `.claude/plugins/` and user
 * `~/.claude/plugins/` roots (one level deep, like Claude Code's marketplace
 * checkouts), plus the project-local `.claude-plugin/` source directory.
 */

import fs from "node:fs"
import path from "node:path"

import {
  CLAUDE_PLUGIN_METADATA_DIR,
  PLUGIN_MANIFEST_FILE,
} from "./manifest_paths"

/** Parsed `plugin.json` identity fields. */
export type PluginJson = {
  /** Plugin name; the namespace seed and default plugin id. */
  readonly name: string
  /** Plugin version string; defaults to `0.0.0` when the manifest omits it. */
  readonly version: string
  /** Free-form description. */
  readonly description: string
}

/** One discovered plugin source directory plus its parsed manifest. */
export type PluginSource = {
  /** Absolute or process-relative plugin directory. */
  readonly dir: string
  /** `plugin.json` path relative to the plugin directory. */
  readonly manifestPath: string
  /** Parsed manifest values. */
  readonly manifest: PluginJson
}

/** Options for [`discoverPluginSources`]. */
export type DiscoveryOptions = {
  /** Project directory (defaults to `process.cwd()`). */
  readonly cwd?: string
  /** Home directory (defaults to env `HOME`). */
  readonly home?: string
}

/**
 * Read and validate `plugin.json` for one plugin directory, accepting both
 * the flat and `.claude-plugin/` layouts.
 *
 * Returns `undefined` when neither manifest location exists; throws a
 * descriptive error when a manifest exists but is not a valid plugin.json.
 */
export function readPluginJson(dir: string): PluginSource | undefined {
  const flat = path.join(dir, PLUGIN_MANIFEST_FILE)
  const nested = path.join(dir, CLAUDE_PLUGIN_METADATA_DIR, PLUGIN_MANIFEST_FILE)
  const manifestPath = fs.existsSync(flat)
    ? PLUGIN_MANIFEST_FILE
    : fs.existsSync(nested)
      ? path.join(CLAUDE_PLUGIN_METADATA_DIR, PLUGIN_MANIFEST_FILE)
      : undefined
  if (manifestPath === undefined) {
    return undefined
  }
  const raw = fs.readFileSync(path.join(dir, manifestPath), "utf8")
  const parsed: unknown = JSON.parse(raw)
  if (!isRecord(parsed) || typeof parsed.name !== "string" || parsed.name.length === 0) {
    throw new Error(`${manifestPath} must carry a non-empty "name"`)
  }
  const version = typeof parsed.version === "string" && parsed.version.length > 0 ? parsed.version : "0.0.0"
  const description = typeof parsed.description === "string" ? parsed.description : ""
  return {
    dir,
    manifestPath,
    manifest: { name: parsed.name, version, description },
  }
}

/**
 * Discover plugin sources under the project and user plugin roots.
 *
 * Each immediate subdirectory of `<root>/.claude/plugins/` that contains a
 * readable `plugin.json` (flat or nested layout) is returned, project roots
 * before user roots, each root sorted by directory name.
 */
export function discoverPluginSources(options: DiscoveryOptions = {}): readonly PluginSource[] {
  const cwd = options.cwd ?? process.cwd()
  const home = options.home ?? process.env["HOME"] ?? ""
  const projectRoot = path.join(cwd, ".claude/plugins")
  const userRoot = path.join(home, ".claude/plugins")
  const roots = userRoot === projectRoot ? [projectRoot] : [projectRoot, userRoot]
  const found: PluginSource[] = []
  for (const root of roots) {
    if (!fs.existsSync(root)) {
      continue
    }
    let entries: readonly string[] = []
    try {
      entries = fs.readdirSync(root).sort()
    } catch {
      continue
    }
    for (const entry of entries) {
      const candidate = path.join(root, entry)
      if (!fs.statSync(candidate).isDirectory()) {
        continue
      }
      try {
        const source = readPluginJson(candidate)
        if (source !== undefined) {
          found.push(source)
        }
      } catch {
        // Unreadable or invalid manifests are skipped during discovery; a
        // direct `--plugin-dir` invocation surfaces the error instead.
      }
    }
  }
  return found
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
