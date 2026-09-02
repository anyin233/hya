/** Typed, fail-closed decoding for additive Hya catalog metadata. */

export type CatalogSource = "configured" | "discovered" | "offline"
export type CatalogProviderSource = CatalogSource | "none"
export type CatalogAuth = "credentialed" | "unauthenticated" | "auth_required" | "auth_rejected" | "not_applicable"
export type CatalogResult = "models" | "empty" | "unavailable" | "invalid" | "unsupported" | "offline"

export type CatalogModel = {
  providerID: string
  modelID: string
  source: CatalogSource
  variants: string[]
}
export type CatalogSelection = {
  providerID: string
  modelID: string
}


export type CatalogProvider = {
  id: string
  name: string
  source: CatalogProviderSource
  auth: CatalogAuth
  result: CatalogResult
  models: CatalogModel[]
}

function object(value: unknown): Record<string, unknown> | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined
  return value as Record<string, unknown>
}

function string(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined
}

function source(value: unknown): CatalogSource | undefined {
  if (value === "configured" || value === "discovered" || value === "offline") return value
}

function providerSource(value: unknown): CatalogProviderSource {
  if (value === "configured" || value === "discovered" || value === "offline") return value
  return "none"
}

function auth(value: unknown): CatalogAuth {
  if (
    value === "credentialed" ||
    value === "unauthenticated" ||
    value === "auth_required" ||
    value === "auth_rejected" ||
    value === "not_applicable"
  )
    return value
  return "unauthenticated"
}

function result(value: unknown): CatalogResult {
  if (
    value === "models" ||
    value === "empty" ||
    value === "unavailable" ||
    value === "invalid" ||
    value === "unsupported" ||
    value === "offline"
  )
    return value
  return "unavailable"
}

function variants(value: unknown): string[] {
  if (Array.isArray(value)) return value.filter((item): item is string => typeof item === "string")
  const record = object(value)
  if (!record) return []
  return Object.keys(record)
}

/** Decode one provider list without coercing malformed model rows. */
export function decodeCatalogProviders(value: unknown): CatalogProvider[] {
  if (!Array.isArray(value)) return []
  const providers: CatalogProvider[] = []
  for (const raw of value) {
    const record = object(raw)
    const id = string(record?.id)
    if (!record || !id) continue
    const modelEntries: [string, unknown][] = Array.isArray(record.models)
      ? record.models.map((rawModel) => [string(object(rawModel)?.modelID) ?? "", rawModel])
      : Object.entries(object(record.models) ?? {})
    const models: CatalogModel[] = []
    for (const [modelID, rawModel] of modelEntries) {
      const model = object(rawModel)
      if (!model || !modelID) continue
      const status = string(model.status)
      if (status === "deprecated") continue
      const modelSource = source(model.source ?? status)
      if (!modelSource) continue
      models.push({
        providerID: id,
        modelID,
        source: modelSource,
        variants: variants(model.variants),
      })
    }
    providers.push({
      id,
      name: string(record.name) ?? id,
      source: providerSource(record.source),
      auth: auth(record.auth),
      result: result(record.result),
      models,
    })
  }
  return providers
}

/** Return a model only when the backend supplied that exact catalog row. */
export function findCatalogModel(
  providers: readonly CatalogProvider[],
  model: { providerID: string; modelID: string },
): CatalogModel | undefined {
  return providers
    .find((provider) => provider.id === model.providerID)
    ?.models.find((row) => row.modelID === model.modelID)
}

/** Validate a local selection against the backend-owned snapshot rows. */
export function isCatalogModelValid(
  providers: unknown,
  model: { providerID: string; modelID: string },
): boolean {
  return !!findCatalogModel(decodeCatalogProviders(providers), model)
}

/** Keep persisted selections that still name an exact backend snapshot row. */
export function filterCatalogSelections(
  providers: unknown,
  selections: readonly CatalogSelection[],
): CatalogSelection[] {
  const decoded = decodeCatalogProviders(providers)
  return selections
    .filter((selection) => !!findCatalogModel(decoded, selection))
    .map((selection) => ({ providerID: selection.providerID, modelID: selection.modelID }))
}

/** Return whether the snapshot has a selectable non-offline model row. */
export function hasLiveCatalogModels(providers: unknown): boolean {
  return decodeCatalogProviders(providers).some((provider) =>
    provider.models.some((model) => model.source !== "offline"),
  )
}

/** Summarize the highest-priority provider startup state without implying health. */
export function catalogStatusSummary(providers: unknown): string {
  const decoded = decodeCatalogProviders(providers)
  const statuses = decoded.filter((provider) => provider.source !== "offline").map(catalogProviderStatus)
  for (const status of ["Authentication required", "Authentication rejected", "Unavailable", "Unsupported"]) {
    if (statuses.includes(status)) return status
  }
  if (decoded.some((provider) => provider.models.some((model) => model.source !== "offline"))) {
    return "Provider configured"
  }
  return decoded.some((provider) => provider.source === "offline") ? "Offline" : "No models"
}

/** Decode a row-backed process default and reject stale or synthetic values. */
export function decodeCatalogSelection(value: unknown, providers: unknown): CatalogSelection | undefined {
  const record = object(value)
  const providerID = string(record?.providerID)
  const modelID = string(record?.modelID)
  if (!providerID || !modelID) return undefined
  const selection = { providerID, modelID }
  return isCatalogModelValid(providers, selection) ? selection : undefined
}

/** Human-readable non-health status for provider/connect presentation. */
export function catalogProviderStatus({ auth, result, source }: CatalogProvider): string {
  if (auth === "auth_required") return "Authentication required"
  if (auth === "auth_rejected") return "Authentication rejected"
  if (result === "offline" || source === "offline") return "Offline"
  if (result === "unsupported") return "Unsupported"
  if (result === "unavailable" || result === "invalid") return "Unavailable"
  if (result === "empty") return "No models"
  if (source === "discovered") return "Discovered"
  if (source === "configured") return "Configured"
  return "No models"
}
