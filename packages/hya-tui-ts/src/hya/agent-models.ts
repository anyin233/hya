/** Normalized frontend boundary for backend-owned Agent model preferences. */

import { decodeCatalogProviders, findCatalogModel } from "./model-catalog"

/** Stable base-model identity used by Agent preference rows and mutations. */
export type AgentModelIdentity = {
  providerID: string
  modelID: string
}

/** Source of the effective model selected for one catalog Agent. */
export type AgentModelSource = "configured" | "remembered" | "default"

/** Supported catalog Agent modes published by the backend. */
export type AgentModelMode = "primary" | "subagent" | "system"

/** Effective model and its precedence source for one Agent. */
export type AgentModelEffective = AgentModelIdentity & {
  source: AgentModelSource
}

/** Normalized Agent model state exposed to the TUI. */
export type AgentModelState = {
  agentID: string
  description?: string
  mode: AgentModelMode
  hidden: boolean
  settable: boolean
  configured: boolean
  preference?: AgentModelIdentity
  preferenceAvailable: boolean
  effective: AgentModelEffective
}

/** Dialog option for choosing a catalog Agent whose model can be configured. */
export type AgentModelTargetOption = {
  value: string
  title: string
  description: string
  category: "Main" | "Subagent" | "System"
  disabled: boolean
}
/**
 * Return a plain object for unknown JSON values.
 * @param value Unknown boundary value.
 * @returns The object record, or undefined when malformed.
 */
function object(value: unknown): Record<string, unknown> | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined
  return value as Record<string, unknown>
}

/**
 * Return a bounded non-empty JSON identity string.
 * @param value Unknown boundary value.
 * @param maximum Maximum accepted UTF-16 code units.
 * @returns The exact string, or undefined when malformed.
 */
function boundedIdentityString(value: unknown, maximum: number): string | undefined {
  return typeof value === "string" && value.trim().length > 0 && value.length <= maximum ? value : undefined
}

/**
 * Decode one allowlisted provider/model identity.
 * @param value Unknown identity payload.
 * @returns The normalized identity, or undefined when malformed.
 */
function decodeIdentity(value: unknown): AgentModelIdentity | undefined {
  const record = object(value)
  const providerID = boundedIdentityString(record?.providerID, 1_024)
  const modelID = boundedIdentityString(record?.modelID, 4_096)
  if (!providerID || !modelID) return undefined
  return { providerID, modelID }
}


/**
 * Decode an effective-model source tag.
 * @param value Unknown source value.
 * @returns The supported source, or undefined.
 */
function decodeSource(value: unknown): AgentModelSource | undefined {
  if (value === "configured" || value === "remembered" || value === "default") return value
  return undefined
}

/**
 * Decode a backend Agent mode tag.
 * @param value Unknown mode value.
 * @returns The supported mode, or undefined.
 */
function decodeMode(value: unknown): AgentModelMode | undefined {
  if (value === "primary" || value === "subagent" || value === "system") return value
  return undefined
}

/**
 * Decode normalized backend Agent model rows and discard malformed or unknown data.
 *
 * @param value - Unknown `agentModels` JSON array from bootstrap or the dedicated endpoint
 * @param providers - Unknown provider catalog used to mark stale preferences unavailable
 * @returns Allowlisted normalized Agent model rows in backend order
 */
export function decodeAgentModels(value: unknown, providers: unknown): AgentModelState[] {
  if (!Array.isArray(value)) return []

  // Decode the provider catalog once so stale preference checks use one snapshot.
  const catalog = decodeCatalogProviders(providers)
  const rows: AgentModelState[] = []

  for (const raw of value) {
    const record = object(raw)
    if (!record) continue

    const agentID = boundedIdentityString(record.agentID, 1_024)
    const mode = decodeMode(record.mode)
    const effectiveRecord = object(record.effective)
    const effectiveIdentity = decodeIdentity(effectiveRecord)
    const effectiveSource = decodeSource(effectiveRecord?.source)
    if (
      !agentID ||
      !mode ||
      typeof record.hidden !== "boolean" ||
      typeof record.settable !== "boolean" ||
      typeof record.configured !== "boolean" ||
      typeof record.preferenceAvailable !== "boolean" ||
      !effectiveIdentity ||
      !effectiveSource ||
      !(record.preference === null || object(record.preference))
    ) {
      continue
    }

    const preference = record.preference === null ? undefined : decodeIdentity(record.preference)
    if (record.preference !== null && !preference) continue
    const available = preference !== undefined && findCatalogModel(catalog, preference) !== undefined
    const preferenceAvailable = record.preferenceAvailable && available
    if (
      record.configured
        ? record.settable || effectiveSource !== "configured"
        : !record.settable || effectiveSource === "configured"
    ) {
      continue
    }
    if (
      effectiveSource === "remembered" &&
      (!preference ||
        !preferenceAvailable ||
        preference.providerID !== effectiveIdentity.providerID ||
        preference.modelID !== effectiveIdentity.modelID)
    ) {
      continue
    }
    if (effectiveSource === "default" && preferenceAvailable) continue
    const row: AgentModelState = {
      agentID,
      ...(typeof record.description === "string" ? { description: record.description } : {}),
      mode,
      hidden: record.hidden,
      configured: record.configured,
      settable: record.settable,
      ...(preference ? { preference } : {}),
      preferenceAvailable,
      effective: {
        providerID: effectiveIdentity.providerID,
        modelID: effectiveIdentity.modelID,
        source: effectiveSource,
      },
    }
    rows.push(row)
  }

  return rows
}

/**
 * Check whether bootstrap capabilities advertise the dedicated Agent preference API.
 *
 * @param capabilities - Unknown bootstrap capability metadata
 * @returns True only for an exact boolean `agentModelPreferences: true` flag
 */
export function supportsAgentModelPreferences(capabilities: unknown): boolean {
  const record = object(capabilities)
  return record?.agentModelPreferences === true
}

/**
 * Convert normalized Agent rows to target-dialog options while retaining disabled rows.
 *
 * @param rows - Normalized Agent model rows from synchronized state
 * @returns One display option per catalog Agent, ordered Main, Subagent, System
 */
export function agentModelTargetOptions(rows: readonly AgentModelState[]): AgentModelTargetOption[] {
  const options = rows.map((row) => {
    const details: string[] = []
    if (row.description) details.push(row.description)
    if (row.configured) details.push("Configured by Agent policy")
    if (row.preference && !row.preferenceAvailable) details.push("stale preference")
    details.push(`${row.effective.source}: ${row.effective.providerID}/${row.effective.modelID}`)
    if (row.hidden) details.push("internal")

    return {
      value: row.agentID,
      title: row.agentID,
      description: details.join(" · "),
      category: row.hidden ? "System" : row.mode === "primary" ? "Main" : "Subagent",
      disabled: row.configured || !row.settable,
    } satisfies AgentModelTargetOption
  })
  const categoryOrder = { Main: 0, Subagent: 1, System: 2 } as const
  options.sort((left, right) => categoryOrder[left.category] - categoryOrder[right.category])
  return options
}

/**
 * Build the title for a model picker targeted at one Agent.
 *
 * @param agentID - Stable catalog Agent id being configured
 * @returns Human-readable targeted picker title
 */
export function agentModelPickerTitle(agentID: string): string {
  return `Select model for ${agentID}`
}
