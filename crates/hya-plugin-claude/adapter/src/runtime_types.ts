/** Shared runtime types for the Claude adapter request loop. */

import fs from "node:fs"
import path from "node:path"
import type { TranslatedSkill, Translation } from "./translate"
import type { ClaudeHookGroup, HookName } from "./hooks"

/** Minimal text sink for stdout/stderr writes. */
export type TextSink = {
  readonly write: (data: string) => unknown
}

/** Adapter environment (injectable for tests). */
export type RuntimeEnv = Readonly<Record<string, string | undefined>>

/** Adapter startup options (mirrors the Bun extension adapter). */
export type RuntimeOptions = {
  readonly input: ReadableStream<Uint8Array>
  readonly stdout: TextSink
  readonly stderr: TextSink
  readonly version: string
  /** Configured plugin id echoed on the initialize reply. */
  readonly pluginId: string
  /** Plugin source directory from `--plugin-dir`, when supplied. */
  readonly pluginDir?: string
  /** Prepared bundle runtime snapshot from `--bundle-runtime`. */
  readonly bundleRuntime?: string
  readonly env?: RuntimeEnv
}

/** One handled request: the serialized reply (or "") and whether to exit. */
export type HandledRequest = {
  readonly response: string
  readonly shouldExit: boolean
}

/** Mutable per-process request context. */
export type RequestContext = {
  readonly version: string
  readonly pluginId: string
  readonly pluginDir: string | undefined
  readonly env: RuntimeEnv
  readonly stderr: TextSink
  /** Translation populated by initialize. */
  translation?: Pick<Translation, "skills" | "hookGroups">
  /** Preloaded translation from an installed bundle snapshot. */
  readonly bundledTranslation?: {
    readonly skills: readonly TranslatedSkill[]
    readonly hookGroups: Readonly<Record<HookName, readonly ClaudeHookGroup[]>>
  }
  /** Private materialized bundle root used for hook cwd and placeholders. */
  readonly bundleRoot?: string
}

/** Build the request context from the runtime options. */
export function createRequestContext(options: RuntimeOptions): RequestContext {
  return {
    version: options.version,
    pluginId: options.pluginId,
    pluginDir: options.pluginDir,
    bundledTranslation: options.bundleRuntime === undefined
      ? undefined
      : loadBundleRuntime(options.bundleRuntime),
    bundleRoot: options.bundleRuntime === undefined
      ? undefined
      : path.join(path.dirname(path.dirname(options.bundleRuntime)), "claude-plugin"),
    env: options.env ?? process.env,
    stderr: options.stderr,
  }
}

function loadBundleRuntime(filePath: string): RequestContext["bundledTranslation"] {
  const document = JSON.parse(fs.readFileSync(filePath, "utf8")) as Record<string, unknown>
  if (document["format_version"] !== 1 || !Array.isArray(document["skills"]) || typeof document["hookGroups"] !== "object" || document["hookGroups"] === null) {
    throw new Error(`${filePath} is not a Claude bundle runtime snapshot`)
  }
  return {
    skills: document["skills"] as readonly TranslatedSkill[],
    hookGroups: document["hookGroups"] as Readonly<Record<HookName, readonly ClaudeHookGroup[]>>,
  }
}
