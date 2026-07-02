import type { CompatHooks } from "./loader/init"
import type { CompatToolDefinition } from "./tool"

export type TextSink = {
  readonly write: (data: string) => unknown
}

export type RuntimeEnv = Readonly<Record<string, string | undefined>>

export type RuntimeOptions = {
  readonly input: ReadableStream<Uint8Array>
  readonly stdout: TextSink
  readonly stderr: TextSink
  readonly version: string
  readonly env?: RuntimeEnv
}

export type HandledRequest = {
  readonly response: string
  readonly shouldExit: boolean
}

export type RequestContext = {
  readonly version: string
  readonly env: RuntimeEnv
  readonly stderr: TextSink
  readonly hooks: CompatHooks[]
  readonly tools: Map<string, CompatToolDefinition>
}

export function createRequestContext(options: RuntimeOptions): RequestContext {
  return {
    version: options.version,
    env: options.env ?? process.env,
    stderr: options.stderr,
    hooks: [],
    tools: new Map<string, CompatToolDefinition>(),
  }
}
