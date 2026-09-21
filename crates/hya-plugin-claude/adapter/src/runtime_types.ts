/** Shared runtime types for the Claude adapter request loop. */

import type { Translation } from "./translate"

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
  translation?: Translation
}

/** Build the request context from the runtime options. */
export function createRequestContext(options: RuntimeOptions): RequestContext {
  return {
    version: options.version,
    pluginId: options.pluginId,
    pluginDir: options.pluginDir,
    env: options.env ?? process.env,
    stderr: options.stderr,
  }
}
