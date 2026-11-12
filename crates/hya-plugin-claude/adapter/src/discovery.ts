/** Read the explicitly selected Claude plugin source directory. */

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

/** One explicitly selected plugin source directory plus its parsed manifest. */
export type PluginSource = {
  /** Absolute or process-relative plugin directory. */
  readonly dir: string
  /** `plugin.json` path relative to the plugin directory. */
  readonly manifestPath: string
  /** Parsed manifest values. */
  readonly manifest: PluginJson
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
