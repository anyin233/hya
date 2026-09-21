/**
 * Claude adapter CLI (P7).
 *
 * Modes:
 *
 * - `bun run src/main.ts --plugin-dir <dir> [--plugin-id <id>]`
 *   Run the JSON-RPC stdio adapter for one Claude Code plugin directory.
 *   `--plugin-id` is the hya-configured plugin id echoed on the initialize
 *   reply (the host enforces the match).
 * - `bun run src/main.ts --emit-bundle-manifest --plugin-dir <dir>`
 *   Offline translation for `hya-backend bundle install --claude`: print one
 *   JSON envelope `{manifest, files}` (see `hya-plugin-claude::emit`) on
 *   stdout and exit 0.
 */

import path from "node:path"

import { TranslateError, translatePlugin } from "./translate"
import { runAdapter } from "./runtime"

const VERSION = "1.0.0"

function printHelp(): void {
  console.log(`hya-claude-adapter ${VERSION}`)
  console.log(
    "Usage: bun run src/main.ts (--plugin-dir <dir> [--plugin-id <id>] | --emit-bundle-manifest --plugin-dir <dir>)",
  )
}

type StartupOptions =
  | { readonly kind: "run"; readonly pluginDir: string; readonly pluginId: string }
  | { readonly kind: "emit"; readonly pluginDir: string }
  | { readonly kind: "help" }
  | { readonly kind: "version" }
  | { readonly kind: "error"; readonly message: string }

function parseStartupArgs(args: readonly string[]): StartupOptions {
  const normalized = args[0] === "--" ? args.slice(1) : args
  if (normalized.length === 1 && normalized[0] === "--version") {
    return { kind: "version" }
  }
  if (normalized.length === 1 && (normalized[0] === "--help" || normalized[0] === "-h")) {
    return { kind: "help" }
  }
  let pluginDir: string | undefined
  let pluginId = "claude"
  let emit = false
  let positional = false
  for (let index = 0; index < normalized.length; index += 1) {
    const flag = normalized[index]
    if (flag === "--plugin-dir") {
      const value = normalized[index + 1]
      if (value === undefined || value.startsWith("--")) {
        return { kind: "error", message: "--plugin-dir requires a directory argument" }
      }
      pluginDir = value
      index += 1
      continue
    }
    if (flag === "--plugin-id") {
      const value = normalized[index + 1]
      if (value === undefined || value.startsWith("--")) {
        return { kind: "error", message: "--plugin-id requires an id argument" }
      }
      pluginId = value
      index += 1
      continue
    }
    if (flag === "--emit-bundle-manifest") {
      emit = true
      continue
    }
    positional = true
  }
  if (positional || pluginDir === undefined) {
    return { kind: "error", message: "expected --plugin-dir <dir> (and optionally --plugin-id <id>)" }
  }
  if (!path.isAbsolute(pluginDir)) {
    pluginDir = path.resolve(process.cwd(), pluginDir)
  }
  return emit ? { kind: "emit", pluginDir } : { kind: "run", pluginDir, pluginId }
}

/** Print the offline `--emit-bundle-manifest` envelope for one plugin dir. */
export function emitBundleManifest(pluginDir: string): void {
  const translation = translatePlugin(pluginDir)
  const envelope = {
    manifest: translation.manifestYaml,
    files: translation.files,
  }
  console.log(JSON.stringify(envelope, null, 2))
}

const startup = parseStartupArgs(Bun.argv.slice(2))
switch (startup.kind) {
  case "version":
    console.log(VERSION)
    process.exit(0)
  case "help":
    printHelp()
    process.exit(0)
  case "error":
    console.error(startup.message)
    process.exit(1)
  case "emit":
    try {
      emitBundleManifest(startup.pluginDir)
      process.exit(0)
    } catch (error) {
      console.error(error instanceof TranslateError ? error.message : String(error))
      process.exit(1)
    }
  case "run":
    await runAdapter({
      input: Bun.stdin.stream(),
      stdout: { write: (data) => process.stdout.write(data) },
      stderr: { write: (data) => process.stderr.write(data) },
      version: VERSION,
      pluginId: startup.pluginId,
      pluginDir: startup.pluginDir,
      env: process.env,
    })
}
