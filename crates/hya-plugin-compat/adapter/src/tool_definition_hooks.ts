import type { CompatHooks } from "./loader/init"

type ToolDefinitionHook = (
  input: { readonly toolID: string },
  output: { description: string; parameters: unknown },
) => unknown | Promise<unknown>

type WireToolDefinition = Readonly<Record<string, unknown>> & {
  readonly name: string
  readonly description: string
  readonly input_schema: unknown
}

export async function runToolDefinitionHooks(
  hooks: readonly CompatHooks[],
  tools: readonly unknown[],
): Promise<readonly unknown[]> {
  const transformed: unknown[] = []
  for (const tool of tools) {
    transformed.push(await transformedTool(hooks, tool))
  }
  return transformed
}

async function transformedTool(
  hooks: readonly CompatHooks[],
  tool: unknown,
): Promise<unknown> {
  if (!isWireToolDefinition(tool)) {
    return tool
  }
  const output = {
    description: tool.description,
    parameters: tool.input_schema,
  }
  for (const hook of hooks) {
    const candidate = hook["tool.definition"]
    if (!isToolDefinitionHook(candidate)) {
      continue
    }
    try {
      await candidate({ toolID: tool.name }, output)
    } catch (caught) {
      if (caught instanceof Error) {
        continue
      }
      throw caught
    }
  }
  return {
    ...tool,
    description: output.description,
    input_schema: output.parameters,
  }
}

function isToolDefinitionHook(value: unknown): value is ToolDefinitionHook {
  return typeof value === "function"
}

function isWireToolDefinition(value: unknown): value is WireToolDefinition {
  if (!isRecord(value)) {
    return false
  }
  return (
    typeof value["name"] === "string" &&
    typeof value["description"] === "string" &&
    "input_schema" in value
  )
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
