import { z } from "zod"

import type { CompatHooks } from "./loader/init"

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

export type ToolRegistry = {
  readonly infos: readonly ToolInfo[]
  readonly tools: ReadonlyMap<string, CompatToolDefinition>
}

export type CompatToolDefinition = {
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
    super("Compat tool context ask() is not supported by hya adapter yet")
  }
}

export function buildToolRegistry(hooks: readonly CompatHooks[]): ToolRegistry {
  const infos: ToolInfo[] = []
  const tools = new Map<string, CompatToolDefinition>()
  for (const hook of hooks) {
    const tool = hook.tool
    if (!isRecord(tool)) {
      continue
    }
    for (const [name, value] of Object.entries(tool)) {
      if (tools.has(name) || !isToolDefinition(value)) {
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
  return { infos, tools }
}

export async function callRegisteredTool(
  registry: ReadonlyMap<string, CompatToolDefinition>,
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

function inputSchemaFromArgs(args: unknown): unknown {
  if (args === undefined) {
    return { type: "object", properties: {}, required: [] }
  }
  if (!isRecord(args)) {
    return { type: "object", properties: {}, required: [] }
  }
  const entries = Object.entries(args)
  if (entries.every((entry) => isZodType(entry[1]))) {
    const shape: Record<string, z.ZodType> = {}
    for (const [key, value] of entries) {
      if (isZodType(value)) {
        shape[key] = value
      }
    }
    return normalizeJsonSchema(z.toJSONSchema(z.object(shape), { io: "input" }))
  }
  const properties: Record<string, unknown> = {}
  for (const [key, value] of entries) {
    if (isJsonSchemaDefinition(value)) {
      properties[key] = value
    }
  }
  return {
    type: "object",
    properties,
    required: Object.keys(properties),
  }
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

function isToolDefinition(value: unknown): value is CompatToolDefinition {
  return (
    isRecord(value) &&
    typeof value.description === "string" &&
    typeof value.execute === "function"
  )
}

function isJsonSchemaDefinition(value: unknown): boolean {
  return typeof value === "boolean" || isRecord(value)
}

function isZodType(value: unknown): value is z.ZodType {
  return value instanceof z.ZodType
}

function normalizeJsonSchema(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map((item) => normalizeJsonSchema(item))
  }
  if (!isRecord(value)) {
    return value
  }
  const normalized: Record<string, unknown> = {}
  for (const [key, item] of Object.entries(value)) {
    if (key === "$schema") {
      continue
    }
    normalized[key] = normalizeJsonSchema(item)
  }
  return normalized
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
