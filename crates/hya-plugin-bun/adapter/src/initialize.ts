import {
  loadExtensionContributions,
  type ExtensionLoadError,
} from "./loader/init"
import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import { hookRegistrationsFrom } from "./registration"
import type {
  ActivationMetadata,
  HandledRequest,
  RequestContext,
} from "./runtime_types"
import { buildToolRegistry } from "./tool"
import { isNonEmptyString, isRecord, ok, type ValidationResult } from "./validate"

export const PROTOCOL_VERSION = 1

const PLUGIN_KIND = "bun"

export async function handleInitialize(
  request: JsonRpcRequest,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = validateInitializeParams(request.params)
  if (!params.ok) {
    return {
      response: errorResponse(request.id, ERROR_CODES.INVALID_PARAMS, params.message),
      shouldExit: false,
    }
  }
  const activation: ActivationMetadata | undefined =
    params.value.activation_id !== undefined && params.value.lifecycle !== undefined
      ? {
          activation_id: params.value.activation_id,
          lifecycle: params.value.lifecycle,
        }
      : undefined
  // Only explicit startup extension paths are loadable. Agent activations and
  // generation-owned shared Plugins use the same closed declaration path;
  // a startup with no paths still declares an empty contribution set.
  const loaded =
    context.extensions.length === 0
      ? { hooks: [], skills: [], workspaceAdapters: [], errors: [] as readonly ExtensionLoadError[] }
      : await loadExtensionContributions(context.extensions, context.env)
  if (loaded.errors.length > 0) {
    return {
      response: errorResponse(
        request.id,
        ERROR_CODES.INTERNAL_ERROR,
        loaded.errors
          .map((error) => `${error.spec}: ${error.message}`)
          .join("; "),
      ),
      shouldExit: false,
    }
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
  const contributions = {
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
        id: context.pluginId,
        version: context.version,
        kind: PLUGIN_KIND,
      },
      ...contributions,
    }),
    shouldExit: false,
  }
}

type InitializeParams = {
  readonly activation_id?: string
  readonly lifecycle?: "transient" | "resident"
}

function validateInitializeParams(value: unknown): ValidationResult<InitializeParams> {
  if (!isRecord(value)) {
    return { ok: false, message: "params must be an object" }
  }
  if (value.protocol_version !== PROTOCOL_VERSION) {
    return {
      ok: false,
      message: `params.protocol_version must be ${PROTOCOL_VERSION}`,
    }
  }
  const host = value.host
  if (
    !isRecord(host) ||
    !isNonEmptyString(host.name) ||
    !isNonEmptyString(host.version)
  ) {
    return {
      ok: false,
      message: "params.host must carry name and version strings",
    }
  }
  const activation_id = value.activation_id
  const lifecycle = value.lifecycle
  if ((activation_id === undefined) !== (lifecycle === undefined)) {
    return {
      ok: false,
      message: "activation_id and lifecycle must be supplied together",
    }
  }
  if (activation_id !== undefined && !isNonEmptyString(activation_id)) {
    return { ok: false, message: "params.activation_id must be a non-empty string" }
  }
  if (lifecycle !== undefined && lifecycle !== "transient" && lifecycle !== "resident") {
    return {
      ok: false,
      message: "params.lifecycle must be \"transient\" or \"resident\"",
    }
  }
  if (activation_id === undefined || lifecycle === undefined) {
    return ok({})
  }
  return ok({
    activation_id,
    lifecycle: lifecycle as "transient" | "resident",
  })
}
