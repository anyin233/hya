import { $ } from "bun"
import { z } from "zod"

import {
  createOpenCodeClientAdapter,
  createOpenCodeProject,
} from "./client_adapter"
import {
  AdapterOptionsParseError,
  discoverPluginSpecs,
  parseAdapterOptions,
} from "./loader/discovery"
import { loadLocalPluginHooks, type OpenCodeHooks } from "./loader/init"
import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import { hookRegistrationsFrom } from "./registration"
import type { HandledRequest, RequestContext, RuntimeEnv } from "./runtime_types"
import { buildToolRegistry } from "./tool"

export const PROTOCOL_VERSION = 1

const InitializeParamsSchema = z
  .object({
    protocol_version: z.literal(PROTOCOL_VERSION),
    host: z.object({
      name: z.string(),
      version: z.string(),
    }),
  })
  .strict()

type LoadedHooksResult =
  | {
      readonly hooks: readonly OpenCodeHooks[]
      readonly workspaceAdapters: readonly WorkspaceAdapterEntry[]
      readonly response?: undefined
    }
  | { readonly hooks?: undefined; readonly response: HandledRequest }

type WorkspaceAdapterEntry = {
  readonly type: string
  readonly name: string
  readonly description: string
}

export async function handleInitialize(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = InitializeParamsSchema.safeParse(request.params)
  if (!params.success) {
    return {
      response: errorResponse(
        request.id,
        ERROR_CODES.INVALID_PARAMS,
        params.error.message,
      ),
      shouldExit: false,
    }
  }
  const loaded = await loadConfiguredHooks(context, request.id)
  if (loaded.response !== undefined) {
    return loaded.response
  }
  const registry = buildToolRegistry(loaded.hooks)
  context.hooks.splice(0, context.hooks.length, ...loaded.hooks)
  context.tools.clear()
  for (const [name, tool] of registry.tools) {
    context.tools.set(name, tool)
  }
  return {
    response: okResponse(request.id, {
      protocol_version: PROTOCOL_VERSION,
      plugin: {
        id: "opencode",
        version: context.version,
        kind: "opencode",
      },
      hooks: hookRegistrationsFrom(loaded.hooks),
      tools: registry.infos,
      workspaceAdapters: loaded.workspaceAdapters,
    }),
    shouldExit: false,
  }
}

async function loadConfiguredHooks(
  context: RequestContext,
  id: number,
): Promise<LoadedHooksResult> {
  let options: ReturnType<typeof parseAdapterOptions>
  try {
    options = parseAdapterOptions(context.env.YACA_OPENCODE_OPTIONS_JSON)
  } catch (error) {
    if (error instanceof AdapterOptionsParseError) {
      return {
        response: {
          response: errorResponse(id, ERROR_CODES.INVALID_PARAMS, error.message),
          shouldExit: false,
        },
      }
    }
    throw error
  }
  if (envFlag(context.env.OPENCODE_PURE)) {
    return { hooks: [], workspaceAdapters: [] }
  }
  const workspaceAdapters: WorkspaceAdapterEntry[] = []
  const directory = context.env.YACA_DIRECTORY ?? process.cwd()
  const worktree = context.env.YACA_WORKTREE ?? directory
  const discovered = await discoverPluginSpecs({
    directory,
    worktree,
    customConfigFile: nonemptyEnv(context.env.OPENCODE_CONFIG),
    customConfigDir: nonemptyEnv(context.env.OPENCODE_CONFIG_DIR),
    disableProjectConfig: envFlag(context.env.OPENCODE_DISABLE_PROJECT_CONFIG),
    inlineConfig: nonemptyEnv(context.env.OPENCODE_CONFIG_CONTENT),
    xdgConfigHome: context.env.XDG_CONFIG_HOME,
    home: context.env.HOME,
  })
  const loaded = await loadLocalPluginHooks(
    [...discovered, ...options.plugin],
    pluginInput(context.env, context.stderr, directory, worktree, workspaceAdapters),
  )
  for (const error of loaded.errors) {
    await context.stderr.write(`opencode plugin ${error.spec}: ${error.message}\n`)
  }
  return { hooks: loaded.hooks, workspaceAdapters }
}

function nonemptyEnv(value: string | undefined): string | undefined {
  return value === undefined || value.length === 0 ? undefined : value
}

function envFlag(value: string | undefined): boolean {
  return value === "true" || value === "1"
}

function pluginInput(
  env: RuntimeEnv,
  stderr: RequestContext["stderr"],
  directory: string,
  worktree: string,
  workspaceAdapters: WorkspaceAdapterEntry[],
): Readonly<Record<string, unknown>> {
  const project = createOpenCodeProject(env, worktree)
  return {
    client: createOpenCodeClientAdapter(stderr, {
      env,
      directory,
      worktree,
      project,
    }),
    directory,
    worktree,
    project,
    serverUrl: new URL(env.YACA_SERVER_URL ?? "http://127.0.0.1:0"),
    $,
    experimental_workspace: {
      register: (type: string, adapter: unknown) => {
        const entry = workspaceAdapterEntry(type, adapter)
        if (entry !== undefined) {
          workspaceAdapters.push(entry)
        }
      },
    },
  }
}

function workspaceAdapterEntry(
  type: string,
  adapter: unknown,
): WorkspaceAdapterEntry | undefined {
  if (!isRecord(adapter)) {
    return undefined
  }
  const name = adapter.name
  const description = adapter.description
  if (typeof name !== "string" || typeof description !== "string") {
    return undefined
  }
  return { type, name, description }
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
