import { $ } from "bun"
import { z } from "zod"

import {
  createCompatClientAdapter,
  createCompatProject,
} from "./client_adapter"
import {
  AdapterOptionsParseError,
  discoverPluginSpecs,
  parseAdapterOptions,
} from "./loader/discovery"
import {
  loadLocalPluginContributions,
  PluginDeclarationError,
  type CompatHooks,
} from "./loader/init"
import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import { hookRegistrationsFrom } from "./registration"
import type {
  PluginContributionSet,
  SkillContribution,
  WorkspaceAdapterContribution,
} from "./contributions"
import type {
  ActivationMetadata,
  HandledRequest,
  RequestContext,
  RuntimeEnv,
} from "./runtime_types"
import { buildToolRegistry } from "./tool"

export type {
  PluginContributionSet,
  SkillContribution,
  WorkspaceAdapterContribution,
} from "./contributions"


export const PROTOCOL_VERSION = 1

const InitializeParamsSchema = z
  .object({
    protocol_version: z.literal(PROTOCOL_VERSION),
    host: z.object({
      name: z.string(),
      version: z.string(),
    }),
    activation_id: z.string().min(1).optional(),
    lifecycle: z.enum(["transient", "resident"]).optional(),
  })
  .strict()
  .superRefine((params, refinement) => {
    if ((params.activation_id === undefined) !== (params.lifecycle === undefined)) {
      refinement.addIssue({
        code: z.ZodIssueCode.custom,
        message: "activation_id and lifecycle must be supplied together",
        path: ["activation_id"],
      })
    }
  })

type LoadedContributionsResult =
  | {
      readonly hooks: readonly CompatHooks[]
      readonly skills: readonly SkillContribution[]
      readonly workspaceAdapters: readonly WorkspaceAdapterContribution[]
      readonly response?: undefined
    }
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
  const activation: ActivationMetadata | undefined =
    params.data.activation_id !== undefined && params.data.lifecycle !== undefined
      ? {
          activation_id: params.data.activation_id,
          lifecycle: params.data.lifecycle,
        }
      : undefined
  const loaded = await loadConfiguredContributions(context, request.id, activation)
  if (loaded.response !== undefined) {
    return loaded.response
  }
  const registry = buildToolRegistry(loaded.hooks)
  if (registry.errors.length > 0) {
    return {
      response: errorResponse(
        request.id,
        ERROR_CODES.INTERNAL_ERROR,
        registry.errors.map((error) => error.message).join("; "),
      ),
      shouldExit: false,
    }
  }
  const contributions: PluginContributionSet = {
    hooks: hookRegistrationsFrom(loaded.hooks),
    tools: registry.infos,
    skills: loaded.skills,
    workspaceAdapters: loaded.workspaceAdapters,
  }
  context.contributions = contributions
  context.hooks.splice(0, context.hooks.length, ...loaded.hooks)
  context.tools.clear()
  for (const [name, tool] of registry.tools) {
    context.tools.set(name, tool)
  }
  context.activation = activation
  return {
    response: okResponse(request.id, {
      protocol_version: PROTOCOL_VERSION,
      plugin: {
        id: "compat",
        version: context.version,
        kind: "compat",
      },
      ...contributions,
    }),
    shouldExit: false,
  }
}

async function loadConfiguredContributions(
  context: RequestContext,
  id: number,
  activation: ActivationMetadata | undefined,
): Promise<LoadedContributionsResult> {
  if (activation !== undefined) {
    const loaded = await loadLocalPluginContributions(
      context.bundleExtensions,
      Object.freeze({}),
    )
    if (loaded.errors.length > 0) {
      return {
        response: {
          response: errorResponse(
            id,
            ERROR_CODES.INTERNAL_ERROR,
            loaded.errors
              .map((error) => `${error.spec}: ${error.message}`)
              .join("; "),
          ),
          shouldExit: false,
        },
      }
    }
    return { hooks: loaded.hooks, skills: loaded.skills, workspaceAdapters: [] }
  }

  let options: ReturnType<typeof parseAdapterOptions>
  try {
    options = parseAdapterOptions(context.env.HYA_COMPAT_OPTIONS_JSON)
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
  if (envFlag(context.env.COMPAT_PURE)) {
    return { hooks: [], skills: [], workspaceAdapters: [] }
  }
  const workspaceAdapters: WorkspaceAdapterContribution[] = []
  const directory = context.env.HYA_DIRECTORY ?? process.cwd()
  const worktree = context.env.HYA_WORKTREE ?? directory
  const discovered = await discoverPluginSpecs({
    directory,
    worktree,
    customConfigFile: nonemptyEnv(context.env.COMPAT_CONFIG),
    customConfigDir: nonemptyEnv(context.env.COMPAT_CONFIG_DIR),
    disableProjectConfig: envFlag(context.env.COMPAT_DISABLE_PROJECT_CONFIG),
    inlineConfig: nonemptyEnv(context.env.COMPAT_CONFIG_CONTENT),
    xdgConfigHome: context.env.XDG_CONFIG_HOME,
    home: context.env.HOME,
  })
  const loaded = await loadLocalPluginContributions(
    [...discovered, ...options.plugin],
    pluginInput(context.env, context.stderr, directory, worktree, workspaceAdapters),
  )
  const declarationErrors = loaded.errors.filter((error) => error.kind === "declaration")
  if (declarationErrors.length > 0) {
    return {
      response: {
        response: errorResponse(
          id,
          ERROR_CODES.INTERNAL_ERROR,
          declarationErrors
            .map((error) => `${error.spec}: ${error.message}`)
            .join("; "),
        ),
        shouldExit: false,
      },
    }
  }
  for (const error of loaded.errors) {
    await context.stderr.write(`compat plugin ${error.spec}: ${error.message}\n`)
  }
  return { hooks: loaded.hooks, skills: loaded.skills, workspaceAdapters }
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
  workspaceAdapters: WorkspaceAdapterContribution[],
): Readonly<Record<string, unknown>> {

  const project = createCompatProject(env, worktree)
  return {
    client: createCompatClientAdapter(stderr, {
      env,
      directory,
      worktree,
      project,
    }),
    directory,
    worktree,
    project,
    serverUrl: new URL(env.HYA_SERVER_URL ?? "http://127.0.0.1:0"),
    $,
    experimental_workspace: {
      register: (type: string, adapter: unknown) => {
        const entry = workspaceAdapterEntry(type, adapter)
        if (entry === undefined) {
          throw new PluginDeclarationError(
            `invalid workspace adapter declaration for type: ${type}`,
          )
        }
        if (
          workspaceAdapters.some(
            (existing) => existing.type === entry.type && existing.name === entry.name,
          )
        ) {
          throw new PluginDeclarationError(
            `duplicate workspace adapter declaration: ${entry.type}:${entry.name}`,
          )
        }
        workspaceAdapters.push(entry)
      },
    },
  }
}

function workspaceAdapterEntry(
  type: string,
  adapter: unknown,
): WorkspaceAdapterContribution | undefined {
  if (!isRecord(adapter)) {
    return undefined
  }
  const name = adapter.name
  const description = adapter.description
  if (type.length === 0 || typeof name !== "string" || name.length === 0) {
    return undefined
  }
  if (typeof description !== "string") {
    return undefined
  }
  return { type, name, description }
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
