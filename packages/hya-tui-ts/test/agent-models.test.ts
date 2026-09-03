import { expect, test } from "bun:test"
import {
  agentModelPickerTitle,
  agentModelTargetOptions,
  decodeAgentModels,
  supportsAgentModelPreferences,
} from "../src/hya/agent-models"
import { CommandMap, Definitions } from "../src/upstream/config/keybind"

const providers = [
  {
    id: "openai",
    source: "configured",
    auth: "credentialed",
    result: "models",
    models: {
      current: { source: "configured", status: "configured", variants: {} },
      "family/name": { source: "configured", status: "configured", variants: {} },
    },
  },
]

test("agent model decoder accepts normalized rows and strips unknown fields", () => {
  const rows = decodeAgentModels(
    [
      {
        agentID: "build",
        description: "Build agent",
        mode: "primary",
        hidden: false,
        configured: false,
        settable: true,
        preference: { providerID: "openai", modelID: "current", token: "secret" },
        preferenceAvailable: true,
        effective: { providerID: "openai", modelID: "current", source: "remembered", apiKey: "secret" },
      },
      {
        agentID: "title",
        description: null,
        mode: "subagent",
        hidden: true,
        configured: true,
        settable: false,
        preference: null,
        preferenceAvailable: false,
        effective: { providerID: "openai", modelID: "current", source: "configured" },
      },
    ],
    providers,
  )

  expect(rows).toEqual([
    {
      agentID: "build",
      description: "Build agent",
      mode: "primary",
      hidden: false,
      configured: false,
      settable: true,
      preference: { providerID: "openai", modelID: "current" },
      preferenceAvailable: true,
      effective: { providerID: "openai", modelID: "current", source: "remembered" },
    },
    {
      agentID: "title",
      mode: "subagent",
      hidden: true,
      configured: true,
      settable: false,
      preferenceAvailable: false,
      effective: { providerID: "openai", modelID: "current", source: "configured" },
    },
  ])
  expect(JSON.stringify(rows)).not.toContain("secret")
})

test("agent model decoder fails closed for malformed rows and stale preferences", () => {
  const rows = decodeAgentModels(
    [
      {
        agentID: "stale",
        mode: "subagent",
        hidden: false,
        configured: false,
        settable: true,
        preference: { providerID: "openai", modelID: "removed" },
        preferenceAvailable: true,
        effective: { providerID: "openai", modelID: "current", source: "default" },
      },
      {
        agentID: "bad-bool",
        mode: "primary",
        hidden: "false",
        configured: false,
        settable: true,
        preference: null,
        preferenceAvailable: false,
        effective: { providerID: "openai", modelID: "current", source: "default" },
      },
      {
        agentID: "bad-source",
        mode: "primary",
        hidden: false,
        configured: false,
        settable: true,
        preference: null,
        preferenceAvailable: false,
        effective: { providerID: "openai", modelID: "current", source: "guessed" },
      },
      {
        agentID: "unsafe-remembered",
        mode: "subagent",
        hidden: false,
        configured: false,
        settable: true,
        preference: { providerID: "openai", modelID: "removed" },
        preferenceAvailable: true,
        effective: { providerID: "openai", modelID: "removed", source: "remembered" },
      },
    ],
    providers,
  )

  expect(rows).toEqual([
    {
      agentID: "stale",
      mode: "subagent",
      hidden: false,
      configured: false,
      settable: true,
      preference: { providerID: "openai", modelID: "removed" },
      preferenceAvailable: false,
      effective: { providerID: "openai", modelID: "current", source: "default" },
    },
  ])
  expect(supportsAgentModelPreferences({ agentModelPreferences: true })).toBe(true)
  expect(supportsAgentModelPreferences({ agentModelPreferences: "true" })).toBe(false)
  expect(supportsAgentModelPreferences(undefined)).toBe(false)
})

test("agent model decoder preserves model-local slashes and rejects inconsistent bounded rows", () => {
  const remembered = {
    agentID: "slash-model",
    mode: "subagent",
    hidden: false,
    configured: false,
    settable: true,
    preference: { providerID: "openai", modelID: "family/name" },
    preferenceAvailable: true,
    effective: { providerID: "openai", modelID: "family/name", source: "remembered" },
  } as const
  const rows = decodeAgentModels(
    [
      remembered,
      { ...remembered, agentID: "a".repeat(1_025) },
      { ...remembered, agentID: "configured-settable", configured: true, settable: true },
      { ...remembered, agentID: "default-with-live-preference", effective: { ...remembered.effective, source: "default" } },
      { ...remembered, agentID: "oversized-provider", preference: { providerID: "p".repeat(1_025), modelID: "family/name" } },
      { ...remembered, agentID: "oversized-model", preference: { providerID: "openai", modelID: "m".repeat(4_097) } },
    ],
    providers,
  )

  expect(rows).toEqual([
    {
      ...remembered,
      preference: { providerID: "openai", modelID: "family/name" },
      effective: { providerID: "openai", modelID: "family/name", source: "remembered" },
    },
  ])
})

test("Agent models target options keep every catalog Agent and identify configured rows", () => {
  const rows = decodeAgentModels(
    [
      {
        agentID: "build",
        description: "Build agent",
        mode: "primary",
        hidden: false,
        configured: false,
        settable: true,
        preference: null,
        preferenceAvailable: false,
        effective: { providerID: "openai", modelID: "current", source: "default" },
      },
      {
        agentID: "compaction",
        description: "Internal compaction",
        mode: "subagent",
        hidden: true,
        configured: true,
        settable: false,
        preference: null,
        preferenceAvailable: false,
        effective: { providerID: "openai", modelID: "current", source: "configured" },
      },
      {
        agentID: "general",
        description: "General agent",
        mode: "subagent",
        hidden: false,
        configured: false,
        settable: true,
        preference: null,
        preferenceAvailable: false,
        effective: { providerID: "openai", modelID: "current", source: "default" },
      },
      {
        agentID: "stale",
        description: "Stale Agent",
        mode: "subagent",
        hidden: false,
        configured: false,
        settable: true,
        preference: { providerID: "openai", modelID: "removed" },
        preferenceAvailable: false,
        effective: { providerID: "openai", modelID: "current", source: "default" },
      },
    ],
    providers,
  )

  expect(agentModelTargetOptions(rows)).toEqual([
    {
      value: "build",
      title: "build",
      description: "Build agent · default: openai/current",
      category: "Main",
      disabled: false,
    },
    {
      value: "general",
      title: "general",
      description: "General agent · default: openai/current",
      category: "Subagent",
      disabled: false,
    },
    {
      value: "stale",
      title: "stale",
      description: "Stale Agent · stale preference · default: openai/current",
      category: "Subagent",
      disabled: false,
    },
    {
      value: "compaction",
      title: "compaction",
      description:
        "Internal compaction · Configured by Agent policy · configured: openai/current · internal",
      category: "System",
      disabled: true,
    },
  ])
  expect(agentModelPickerTitle("compaction")).toBe("Select model for compaction")
})

test("Agent model command preserves normal Agent cycling", () => {
  expect(CommandMap.agent_models).toBe("agent.model.list")
  expect(Definitions.agent_models.default).toBe("none")
  expect(CommandMap.agent_cycle).toBe("agent.cycle")
  expect(Definitions.agent_cycle.default).toBe("tab")
  expect(CommandMap.agent_cycle_reverse).toBe("agent.cycle.reverse")
  expect(Definitions.agent_cycle_reverse.default).toBe("shift+tab")
})
