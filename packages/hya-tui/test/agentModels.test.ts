import { expect, test } from "bun:test"
import type { AgentModelState } from "../src/client"
import type { KeyLike } from "../src/keys/bindings"
import {
  agentModelHeaderLine,
  agentModelLine,
  agentModelsViewHint,
  agentModelsViewKey,
  effectiveRef,
  initialAgentModelsView,
  notSettableReason,
  settleAgentModelsView,
  shownAgentModels,
  sourceText,
  type AgentModelsViewState,
} from "../src/state/agentModels"

const key = (name: string, extra: Partial<KeyLike> = {}): KeyLike => ({
  name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra,
})

const rows: AgentModelState[] = [
  { agentId: "build", mode: "primary", settable: true, effective: { providerId: "anthropic", modelId: "claude" }, source: "AGENT_MODEL_SOURCE_DEFAULT", preference: {} },
  { agentId: "review", mode: "subagent", settable: true, effective: { providerId: "openai", modelId: "gpt" }, source: "AGENT_MODEL_SOURCE_REMEMBERED", preference: { providerId: "openai", modelId: "gpt" } },
  { agentId: "fixed", mode: "subagent", settable: false, configured: true, effective: { providerId: "google", modelId: "gemini" }, source: "AGENT_MODEL_SOURCE_CONFIGURED" },
]

test("sourceText maps every AgentModelSource", () => {
  expect(sourceText("AGENT_MODEL_SOURCE_SESSION")).toBe("session")
  expect(sourceText("AGENT_MODEL_SOURCE_CONFIGURED")).toBe("configured")
  expect(sourceText("AGENT_MODEL_SOURCE_REMEMBERED")).toBe("remembered")
  expect(sourceText("AGENT_MODEL_SOURCE_DEFAULT")).toBe("default")
  expect(sourceText(undefined)).toBe("—")
})

test("effectiveRef joins provider/model, dash when unset", () => {
  expect(effectiveRef(rows[0]!)).toBe("anthropic/claude")
  expect(effectiveRef({ agentId: "x" })).toBe("—")
})

test("notSettableReason explains configured vs. otherwise unsettable agents", () => {
  expect(notSettableReason(rows[0]!)).toBeUndefined()
  expect(notSettableReason(rows[2]!)).toBe("has direct model/category configuration")
  expect(notSettableReason({ agentId: "x", settable: false })).toBe("cannot take a remembered preference")
})

test("agentModelLine and agentModelHeaderLine fit the width and show the reason for a non-settable agent", () => {
  expect(agentModelLine(rows[0]!, 80)).toContain("build")
  expect(agentModelLine(rows[0]!, 80)).toContain("anthropic/claude")
  expect(agentModelLine(rows[2]!, 200)).toContain("direct model/category configuration")
  expect(agentModelHeaderLine(80)).toContain("AGENT")
  expect(Bun.stringWidth(agentModelLine(rows[0]!, 40))).toBeLessThanOrEqual(40)
})

test("shownAgentModels filters by id, mode, or effective model", () => {
  expect(shownAgentModels({ filter: "" }, rows)).toHaveLength(3)
  expect(shownAgentModels({ filter: "build" }, rows)).toEqual([rows[0]])
  expect(shownAgentModels({ filter: "subagent" }, rows)).toEqual([rows[1], rows[2]])
  expect(shownAgentModels({ filter: "gemini" }, rows)).toEqual([rows[2]])
})

test("initialAgentModelsView highlights the first agent", () => {
  expect(initialAgentModelsView(rows).agent).toBe("build")
  expect(initialAgentModelsView([]).agent).toBeUndefined()
})

test("settleAgentModelsView keeps the highlight when the row still exists", () => {
  const view: AgentModelsViewState = { agent: "review", filter: "", filtering: false }
  expect(settleAgentModelsView(view, rows).agent).toBe("review")
  expect(settleAgentModelsView(view, [rows[0]!]).agent).toBe("build")
})

test("Enter on a settable agent picks a model; on a configured agent it notices why", () => {
  const settable: AgentModelsViewState = { agent: "build", filter: "", filtering: false }
  expect(agentModelsViewKey(settable, key("return"), rows)).toEqual({ type: "pickModel", agent: "build" })
  const configured: AgentModelsViewState = { agent: "fixed", filter: "", filtering: false }
  const outcome = agentModelsViewKey(configured, key("return"), rows)
  expect(outcome).toEqual({ type: "update", view: { ...configured, notice: { tone: "info", text: "fixed has direct model/category configuration" } } })
})

test("c clears a set preference, notices when there is none or the agent is not settable", () => {
  const withPreference: AgentModelsViewState = { agent: "review", filter: "", filtering: false }
  expect(agentModelsViewKey(withPreference, key("c", { sequence: "c" }), rows)).toEqual({ type: "clear", agent: "review" })
  const noPreference: AgentModelsViewState = { agent: "build", filter: "", filtering: false }
  const outcome = agentModelsViewKey(noPreference, key("c", { sequence: "c" }), rows)
  expect(outcome).toEqual({ type: "update", view: { ...noPreference, notice: { tone: "info", text: "build has no remembered preference" } } })
})

test("r refreshes, Esc closes", () => {
  const view: AgentModelsViewState = { agent: "build", filter: "", filtering: false }
  expect(agentModelsViewKey(view, key("r", { sequence: "r" }), rows)).toEqual({ type: "refresh" })
  expect(agentModelsViewKey(view, key("escape"), rows)).toEqual({ type: "close" })
})

test("agentModelsViewHint reflects busy, filtering, and the default key hints", () => {
  expect(agentModelsViewHint({ agent: undefined, filter: "", filtering: false, busy: { label: "Saving", startedAt: 0 } })).toContain("Esc cancels")
  expect(agentModelsViewHint({ agent: undefined, filter: "", filtering: true })).toContain("Type to filter")
  expect(agentModelsViewHint({ agent: undefined, filter: "", filtering: false })).toContain("Esc close")
})
