import { expect, test } from "bun:test"
import {
  catalogProviderStatus,
  catalogStatusSummary,
  decodeCatalogProviders,
  decodeCatalogSelection,
  filterCatalogSelections,
  findCatalogModel,
  hasLiveCatalogModels,
  isCatalogModelValid,
} from "../src/hya/model-catalog"

test("catalog decoder preserves provider status and model provenance", () => {
  const providers = decodeCatalogProviders([
    {
      id: "gateway",
      name: "Gateway",
      source: "discovered",
      auth: "credentialed",
      result: "models",
      models: {
        "gpt-5": { id: "gpt-5", source: "discovered", status: "discovered", variants: { low: {} } },
      },
    },
    {
      id: "needs-auth",
      source: "none",
      auth: "auth_required",
      result: "unavailable",
      models: {},
    },
  ])
  expect(providers[0]?.models[0]).toEqual({
    providerID: "gateway",
    modelID: "gpt-5",
    source: "discovered",
    variants: ["low"],
  })
  expect(catalogProviderStatus(providers[1]!)).toBe("Authentication required")
})

test("stale recent and unknown session models stay outside the snapshot", () => {
  const providers = decodeCatalogProviders([
    {
      id: "hya",
      source: "offline",
      auth: "not_applicable",
      result: "offline",
      models: { offline: { source: "offline", status: "offline", variants: {} } },
    },
  ])
  expect(
    filterCatalogSelections(providers, [
      { providerID: "hya", modelID: "old" },
      { providerID: "hya", modelID: "offline" },
      { providerID: "other", modelID: "offline" },
    ]),
  ).toEqual([{ providerID: "hya", modelID: "offline" }])
  expect(isCatalogModelValid(providers, { providerID: "hya", modelID: "old" })).toBe(false)
  expect(findCatalogModel(providers, { providerID: "other", modelID: "offline" })).toBeUndefined()
})

test("offline row is visible only when supplied by backend", () => {
  expect(decodeCatalogProviders([]).flatMap((provider) => provider.models)).toEqual([])
  const live = decodeCatalogProviders([
    {
      id: "openai",
      source: "configured",
      auth: "unauthenticated",
      result: "models",
      models: { "gpt-5": { source: "configured", status: "configured", variants: {} } },
    },
  ])
  expect(live.flatMap((provider) => provider.models).some((row) => row.source === "offline")).toBe(false)
})

test("configured rows are selectable without claiming discovery connection", () => {
  const providers = [
    {
      id: "anonymous",
      source: "configured",
      auth: "unauthenticated",
      result: "models",
      models: { public: { source: "configured", status: "configured", variants: {} } },
    },
  ]
  expect(hasLiveCatalogModels(providers)).toBe(true)
  expect(catalogStatusSummary(providers)).toBe("Provider configured")
})

test("provider failures have clear non-health labels", () => {
  const authRequired = [{ id: "private", source: "none", auth: "auth_required", result: "unavailable", models: {} }]
  const rejected = [{ id: "private", source: "none", auth: "auth_rejected", result: "unavailable", models: {} }]
  const unsupported = [{ id: "private", source: "none", auth: "unauthenticated", result: "unsupported", models: {} }]
  expect(catalogStatusSummary(authRequired)).toBe("Authentication required")
  expect(catalogStatusSummary(rejected)).toBe("Authentication rejected")
  expect(catalogStatusSummary(unsupported)).toBe("Unsupported")
})

test("row-backed default rejects unknown session or stale model references", () => {
  const providers = [
    {
      id: "gateway",
      source: "discovered",
      auth: "credentialed",
      result: "models",
      models: { current: { source: "discovered", status: "discovered", variants: {} } },
    },
  ]
  expect(decodeCatalogSelection({ providerID: "gateway", modelID: "current" }, providers)).toEqual({
    providerID: "gateway",
    modelID: "current",
  })
  expect(decodeCatalogSelection({ providerID: "gateway", modelID: "stale" }, providers)).toBeUndefined()
  expect(decodeCatalogSelection({ providerID: "session", modelID: "unknown" }, providers)).toBeUndefined()
})

test("malformed metadata fails closed instead of becoming configured", () => {
  const providers = decodeCatalogProviders([
    {
      id: "bad",
      source: "healthy",
      auth: "token-present",
      result: "healthy",
      models: { guessed: { source: "active", status: "active", variants: {} } },
    },
  ])
  expect(providers[0]?.models).toEqual([])
  expect(catalogProviderStatus(providers[0]!)).toBe("Unavailable")
})
