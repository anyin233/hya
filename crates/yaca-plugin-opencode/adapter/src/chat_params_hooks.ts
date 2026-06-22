import type { OpenCodeHooks } from "./loader/init"

export type WireCompletionRequest = {
  readonly model: string
  readonly system?: string
  readonly messages: readonly unknown[]
  readonly tools: readonly unknown[]
  readonly temperature?: number
  readonly max_output_tokens?: number
  readonly reasoning?: string
}

export type ChatParamsParams = {
  readonly session: string
  readonly message: string
  readonly request: WireCompletionRequest
}

export type ChatParamsOutcome = {
  readonly outcome: "continue"
  readonly request: WireCompletionRequest
}

type ChatParamsHook = (
  input: ChatParamsInput,
  output: ChatParamsOutput,
) => unknown | Promise<unknown>

type SystemTransformHook = (
  input: { readonly sessionID?: string; readonly model: OpenCodeModel },
  output: { system: string[] },
) => unknown | Promise<unknown>

type ChatParamsInput = {
  readonly sessionID: string
  readonly agent: string
  readonly model: OpenCodeModel
  readonly provider: ProviderContext
  readonly message: UserMessage
}

type ChatParamsOutput = {
  temperature: number
  topP: number
  topK: number
  maxOutputTokens: number | undefined
  options: Record<string, unknown>
}

type OpenCodeModel = {
  readonly id: string
  readonly providerID: string
  readonly api: { readonly id: string; readonly url: string; readonly npm: string }
  readonly name: string
  readonly capabilities: {
    readonly temperature: boolean
    readonly reasoning: boolean
    readonly attachment: boolean
    readonly toolcall: boolean
    readonly input: Record<string, boolean>
    readonly output: Record<string, boolean>
  }
  readonly cost: { readonly input: number; readonly output: number; readonly cache: Record<string, number> }
  readonly limit: { readonly context: number; readonly output: number }
  readonly status: "active"
  readonly options: Record<string, unknown>
  readonly headers: Record<string, string>
}

type ProviderContext = {
  readonly source: "custom"
  readonly info: {
    readonly id: string
    readonly name: string
    readonly source: "custom"
    readonly env: readonly string[]
    readonly options: Record<string, unknown>
    readonly models: Record<string, OpenCodeModel>
  }
  readonly options: Record<string, unknown>
}

type UserMessage = {
  readonly id: string
  readonly sessionID: string
  readonly role: "user"
  readonly time: { readonly created: number }
  readonly agent: string
  readonly model: { readonly providerID: string; readonly modelID: string }
}

export async function runChatParamsHooks(
  hooks: readonly OpenCodeHooks[],
  params: ChatParamsParams,
): Promise<ChatParamsOutcome> {
  const model = openCodeModel(params.request.model)
  const system = await transformedSystem(hooks, params, model)
  const request = { ...params.request, system }
  const output: ChatParamsOutput = {
    temperature: request.temperature ?? 0,
    topP: 1,
    topK: 0,
    maxOutputTokens: request.max_output_tokens,
    options: {},
  }
  const input = chatParamsInput(params, model)
  for (const hook of hooks) {
    const candidate = hook["chat.params"]
    if (!isChatParamsHook(candidate)) {
      continue
    }
    try {
      await candidate(input, output)
    } catch {
      continue
    }
  }
  return {
    outcome: "continue",
    request: {
      ...request,
      temperature: output.temperature,
      max_output_tokens: output.maxOutputTokens,
    },
  }
}

async function transformedSystem(
  hooks: readonly OpenCodeHooks[],
  params: ChatParamsParams,
  model: OpenCodeModel,
): Promise<string | undefined> {
  const output = { system: params.request.system === undefined ? [] : [params.request.system] }
  for (const hook of hooks) {
    const candidate = hook["experimental.chat.system.transform"]
    if (!isSystemTransformHook(candidate)) {
      continue
    }
    try {
      await candidate({ sessionID: params.session, model }, output)
    } catch {
      continue
    }
  }
  return output.system.length === 0 ? undefined : output.system.join("\n\n")
}

function chatParamsInput(params: ChatParamsParams, model: OpenCodeModel): ChatParamsInput {
  return {
    sessionID: params.session,
    agent: "yaca",
    model,
    provider: providerContext(model),
    message: {
      id: params.message,
      sessionID: params.session,
      role: "user",
      time: { created: 0 },
      agent: "yaca",
      model: { providerID: model.providerID, modelID: model.id },
    },
  }
}

function openCodeModel(modelRef: string): OpenCodeModel {
  const slash = modelRef.indexOf("/")
  const providerID = slash < 0 ? "yaca" : modelRef.slice(0, slash)
  const id = slash < 0 ? modelRef : modelRef.slice(slash + 1)
  return {
    id,
    providerID,
    api: { id: providerID, url: "", npm: "" },
    name: id,
    capabilities: {
      temperature: true,
      reasoning: true,
      attachment: false,
      toolcall: true,
      input: { text: true, audio: false, image: false, video: false, pdf: false },
      output: { text: true, audio: false, image: false, video: false, pdf: false },
    },
    cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
    limit: { context: 0, output: 0 },
    status: "active",
    options: {},
    headers: {},
  }
}

function providerContext(model: OpenCodeModel): ProviderContext {
  return {
    source: "custom",
    info: {
      id: model.providerID,
      name: model.providerID,
      source: "custom",
      env: [],
      options: {},
      models: { [model.id]: model },
    },
    options: {},
  }
}

function isChatParamsHook(value: unknown): value is ChatParamsHook {
  return typeof value === "function"
}

function isSystemTransformHook(value: unknown): value is SystemTransformHook {
  return typeof value === "function"
}
