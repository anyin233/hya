/**
 * Initialize handling for the Claude adapter (P7 7.1).
 *
 * The adapter declares its translated Claude Code resources on the hya
 * plugin ABI v1 initialize reply: skills from `agents/`+`skills/`+
 * `commands/`, and hook registrations derived from `hooks/hooks.json`.
 * Tool declarations stay empty in v1 — Claude Code plugin tools arrive
 * through MCP, not the JS tool API.
 */

import { readPluginJson, type PluginSource } from "./discovery"
import { hookRegistrationsFrom } from "./hooks"
import { CLAUDE_PLUGIN_KIND } from "./manifest_paths"
import { translatePlugin } from "./translate"
import { isNonEmptyString, isRecord, ok, type ValidationResult } from "./validate"
import { ERROR_CODES, errorResponse, okResponse, type JsonRpcRequest } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"

export const PROTOCOL_VERSION = 1

/** Handle one `initialize` request against the configured plugin dir. */
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
  let source: PluginSource
  try {
    const discovered = context.pluginDir === undefined
      ? undefined
      : readPluginJson(context.pluginDir)
    if (discovered === undefined) {
      throw new Error(
        context.pluginDir === undefined
          ? "no --plugin-dir was supplied"
          : `${context.pluginDir} is not a Claude Code plugin (missing plugin.json)`,
      )
    }
    source = discovered
  } catch (error) {
    return {
      response: errorResponse(
        request.id,
        ERROR_CODES.INVALID_PARAMS,
        error instanceof Error ? error.message : String(error),
      ),
      shouldExit: false,
    }
  }
  const translation = translatePlugin(source.dir)
  const hooks = hookRegistrationsFrom({ groups: translation.hookGroups })
  context.translation = translation
  return {
    response: okResponse(request.id, {
      protocol_version: PROTOCOL_VERSION,
      plugin: {
        id: context.pluginId,
        version: context.version,
        kind: CLAUDE_PLUGIN_KIND,
      },
      hooks,
      tools: [],
      skills: translation.skills.map((skill) => ({
        id: skill.id,
        content: skill.content,
        digest: skill.digest,
      })),
      workspaceAdapters: [],
    }),
    shouldExit: false,
  }
}

type InitializeParams = {
  readonly protocolVersion: number
}

function validateInitializeParams(value: unknown): ValidationResult<InitializeParams> {
  if (!isRecord(value)) {
    return { ok: false, message: "params must be an object" }
  }
  if (value["protocol_version"] !== PROTOCOL_VERSION) {
    return {
      ok: false,
      message: `params.protocol_version must be ${PROTOCOL_VERSION}`,
    }
  }
  const host = value["host"]
  if (!isRecord(host) || !isNonEmptyString(host["name"]) || !isNonEmptyString(host["version"])) {
    return {
      ok: false,
      message: "params.host must carry name and version strings",
    }
  }
  return ok({ protocolVersion: PROTOCOL_VERSION })
}
