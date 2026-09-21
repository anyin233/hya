import type { ExtensionHooks } from "./loader/init"

export type ToolInfo = {
  readonly name: string
  readonly description: string
  readonly inputSchema: unknown
}

export type ToolContext = {
  readonly sessionID: string
  readonly messageID: string
  readonly agent: string
  readonly directory: string
  readonly worktree: string
  readonly abort: AbortSignal
  readonly metadata: (input: ToolMetadataInput) => void
  readonly ask: () => Promise<never>
}

export type ToolMetadataInput = {
  readonly title?: string
  readonly metadata?: Readonly<Record<string, unknown>>
}

export type ToolCallParams = {
  readonly tool: string
  readonly session: string
  readonly call: string
  readonly input: unknown
}

export type ToolRuntimeContext = {
  readonly directory: string
  readonly worktree: string
}

export type ToolCallReply = {
  readonly ok: boolean
  readonly output: unknown
  readonly time_ms: number
}

export type ToolDeclarationError = {
  /** Declaration failure category. */
  readonly kind: "duplicate" | "malformed"
  /** Tool name used for diagnostics. */
  readonly name: string
  /** Contextual declaration failure detail. */
  readonly message: string
}

export type ToolRegistry = {
  readonly infos: readonly ToolInfo[]
  readonly tools: ReadonlyMap<string, ExtensionToolDefinition>
  readonly errors: readonly ToolDeclarationError[]
}

export type ExtensionToolDefinition = {
  readonly description: string
  readonly args?: unknown
  readonly execute: (
    args: unknown,
    context: ToolContext,
  ) => unknown | Promise<unknown>
}

type ToolExecutionState = {
  title: string
  metadata: Record<string, unknown>
}

export class UnsupportedToolAskError extends Error {
  readonly name = "UnsupportedToolAskError"

  constructor() {
    super("Bun adapter tool context ask() is not supported yet")
  }
}

export function buildToolRegistry(hooks: readonly ExtensionHooks[]): ToolRegistry {
  const infos: ToolInfo[] = []
  const tools = new Map<string, ExtensionToolDefinition>()
  const errors: ToolDeclarationError[] = []
  for (const hook of hooks) {
    const tool = hook.tool
    if (tool === undefined) {
      continue
    }
    if (!isRecord(tool)) {
      errors.push({
        kind: "malformed",
        name: "<unknown>",
        message: "tool declaration must be an object",
      })
      continue
    }
    for (const [name, value] of Object.entries(tool)) {
      if (tools.has(name)) {
        errors.push({
          kind: "duplicate",
          name,
          message: `duplicate tool declaration: ${name}`,
        })
        continue
      }
      if (!isToolDefinition(value)) {
        errors.push({
          kind: "malformed",
          name,
          message: `malformed tool declaration: ${name}`,
        })
        continue
      }
      tools.set(name, value)
      infos.push({
        name,
        description: value.description,
        inputSchema: inputSchemaFromArgs(value.args),
      })
    }
  }
  return { infos, tools, errors }
}

export async function callRegisteredTool(
  registry: ReadonlyMap<string, ExtensionToolDefinition>,
  params: ToolCallParams,
  context: ToolRuntimeContext,
): Promise<ToolCallReply> {
  const started = performance.now()
  const tool = registry.get(params.tool)
  if (tool === undefined) {
    return failedReply(`unknown tool: ${params.tool}`, started)
  }
  const state: ToolExecutionState = { title: "", metadata: {} }
  try {
    const result = await tool.execute(
      params.input,
      toolContext(params, context, state),
    )
    return {
      ok: true,
      output: normalizeToolResult(result, state),
      time_ms: elapsedMs(started),
    }
  } catch (error) {
    return failedReply(errorMessage(error), started)
  }
}

/**
 * Accept only a plain JSON-Schema object as the declared `args`; anything else
 * collapses to a permissive object schema the host keeps.
 */
function inputSchemaFromArgs(args: unknown): unknown {
  if (args === undefined) {
    return { type: "object", properties: {}, required: [] }
  }
  if (!isRecord(args)) {
    return { type: "object" }
  }
  return args
}

function toolContext(
  params: ToolCallParams,
  context: ToolRuntimeContext,
  state: ToolExecutionState,
): ToolContext {
  return {
    sessionID: params.session,
    messageID: "",
    agent: "",
    directory: context.directory,
    worktree: context.worktree,
    abort: new AbortController().signal,
    metadata: (input) => {
      if (input.title !== undefined) {
        state.title = input.title
      }
      if (input.metadata !== undefined) {
        state.metadata = { ...state.metadata, ...input.metadata }
      }
    },
    ask: async () => {
      throw new UnsupportedToolAskError()
    },
  }
}

function normalizeToolResult(
  result: unknown,
  state: ToolExecutionState,
): unknown {
  if (typeof result === "string") {
    return { title: state.title, output: result, metadata: state.metadata }
  }
  if (!isRecord(result) || typeof result.output !== "string") {
    return { title: state.title, output: String(result), metadata: state.metadata }
  }
  const metadata = isRecord(result.metadata)
    ? { ...state.metadata, ...result.metadata }
    : state.metadata
  const output: Record<string, unknown> = {
    title: typeof result.title === "string" ? result.title : state.title,
    output: result.output,
    metadata,
  }
  if (result.attachments !== undefined) {
    output.attachments = result.attachments
  }
  return output
}

function isToolDefinition(value: unknown): value is ExtensionToolDefinition {
  return (
    isRecord(value) &&
    typeof value.description === "string" &&
    typeof value.execute === "function"
  )
}

function failedReply(message: string, started: number): ToolCallReply {
  return { ok: false, output: message, time_ms: elapsedMs(started) }
}

function elapsedMs(started: number): number {
  return Math.max(0, Math.round(performance.now() - started))
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message
  }
  return String(error)
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
