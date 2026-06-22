import { $ } from "bun"
import { z } from "zod"

import { createOpenCodeClientAdapter } from "./client_adapter"
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
  | { readonly hooks: readonly OpenCodeHooks[]; readonly response?: undefined }
  | { readonly hooks?: undefined; readonly response: HandledRequest }

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
  const directory = context.env.YACA_DIRECTORY ?? process.cwd()
  const worktree = context.env.YACA_WORKTREE ?? directory
  const discovered = await discoverPluginSpecs({
    directory,
    xdgConfigHome: context.env.XDG_CONFIG_HOME,
    home: context.env.HOME,
  })
  const loaded = await loadLocalPluginHooks(
    [...discovered, ...options.plugin],
    pluginInput(context.env, context.stderr, directory, worktree),
  )
  for (const error of loaded.errors) {
    await context.stderr.write(`opencode plugin ${error.spec}: ${error.message}\n`)
  }
  return { hooks: loaded.hooks }
}

function pluginInput(
  env: RuntimeEnv,
  stderr: RequestContext["stderr"],
  directory: string,
  worktree: string,
): Readonly<Record<string, unknown>> {
  return {
    client: createOpenCodeClientAdapter(stderr),
    directory,
    worktree,
    project: {
      id: env.YACA_PROJECT_ID ?? worktree,
      worktree,
      time: {
        created: Date.now(),
      },
    },
    serverUrl: new URL(env.YACA_SERVER_URL ?? "http://127.0.0.1:0"),
    $,
    experimental_workspace: {
      register: () => undefined,
    },
  }
}
