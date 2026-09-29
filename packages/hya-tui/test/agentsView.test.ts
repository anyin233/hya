import { expect, test } from "bun:test"
import type { AgentModelState } from "../src/client"
import type { KeyLike } from "../src/keys/bindings"
import {
  agentHeaderLine,
  agentLine,
  agentsViewHint,
  agentsViewKey,
  agentsViewLines,
  effectiveRef,
  initialAgentsView,
  sectionRule,
  settleAgentsView,
  shownAgents,
  sourceText,
  type AgentsViewState,
} from "../src/state/agentsView"

const key = (name: string, extra: Partial<KeyLike> = {}): KeyLike => ({
  name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra,
})

// Server order is by stable id; the view regroups it into sections.
const rows: AgentModelState[] = [
  { agentId: "hya-main", mode: "primary", settable: false, configured: true, configurationPath: "/c/config.yaml", effective: { providerId: "anthropic", modelId: "claude" }, source: "AGENT_MODEL_SOURCE_CONFIGURED" },
  { agentId: "hya-compaction", mode: "subagent", hidden: true, settable: true, effective: { providerId: "anthropic", modelId: "claude" }, source: "AGENT_MODEL_SOURCE_DEFAULT" },
  { agentId: "hya-scout", mode: "subagent", settable: true, effective: { providerId: "openai", modelId: "gpt" }, source: "AGENT_MODEL_SOURCE_REMEMBERED", preference: { providerId: "openai", modelId: "gpt" } },
  { agentId: "hya-plan", mode: "primary", settable: true, effective: { providerId: "google", modelId: "gemini" }, source: "AGENT_MODEL_SOURCE_DEFAULT", preference: {} },
  { agentId: "hya-reviewer", mode: "subagent", settable: true, effective: { providerId: "openai", modelId: "review" }, source: "AGENT_MODEL_SOURCE_DEFAULT" },
  { agentId: "hya-task", mode: "subagent", settable: true, effective: { providerId: "openai", modelId: "task" }, source: "AGENT_MODEL_SOURCE_DEFAULT" },
  { agentId: "hya-summary", mode: "subagent", hidden: true, settable: true, effective: { providerId: "anthropic", modelId: "claude" }, source: "AGENT_MODEL_SOURCE_DEFAULT" },
  { agentId: "hya-title", mode: "subagent", hidden: true, settable: true, effective: { providerId: "anthropic", modelId: "claude" }, source: "AGENT_MODEL_SOURCE_DEFAULT" },
]

const at = (agent: string): AgentsViewState => ({ agent, filter: "", filtering: false })

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

test("lines group primary, subagent, then hidden system agents under divider rules", () => {
  const lines = agentsViewLines({ filter: "" }, rows)
  expect(lines.map((line) => line.kind === "divider" ? `— ${line.title}` : line.row.agentId)).toEqual([
    "— Primary agents", "hya-main", "hya-plan",
    "— Subagents", "hya-scout", "hya-reviewer", "hya-task",
    "— System agents", "hya-compaction", "hya-summary", "hya-title",
  ])
})

test("a filter drops sections left empty and matches section titles", () => {
  expect(agentsViewLines({ filter: "gpt" }, rows).map((line) => line.kind === "divider" ? line.title : line.row.agentId)).toEqual(["Subagents", "hya-scout"])
  expect(shownAgents({ filter: "system" }, rows).map((row) => row.agentId)).toEqual(["hya-compaction", "hya-summary", "hya-title"])
})

test("Up/Down follow display order across sections and skip dividers", () => {
  const down = (view: AgentsViewState) => {
    const outcome = agentsViewKey(view, key("down"), rows)
    if (outcome.type !== "update") throw new Error(outcome.type)
    return outcome.view
  }
  const order: string[] = []
  let view = at("hya-main")
  for (let i = 0; i < 9; i++) { view = down(view); order.push(view.agent!) }
  expect(order).toEqual(["hya-plan", "hya-scout", "hya-reviewer", "hya-task", "hya-compaction", "hya-summary", "hya-title", "hya-main", "hya-plan"])
  const up = agentsViewKey(at("hya-main"), key("up"), rows)
  expect(up.type === "update" && up.view.agent).toBe("hya-title")
})

test("initialAgentsView opens on the session's agent, else the first primary agent", () => {
  expect(initialAgentsView(rows, "hya-plan").agent).toBe("hya-plan")
  expect(initialAgentsView(rows, "gone").agent).toBe("hya-main")
  expect(initialAgentsView([]).agent).toBeUndefined()
})

test("settleAgentsView keeps the highlight when the row still exists", () => {
  expect(settleAgentsView(at("hya-scout"), rows).agent).toBe("hya-scout")
  expect(settleAgentsView(at("hya-scout"), [rows[3]!]).agent).toBe("hya-plan")
})

test("Enter selects a primary agent; on a subagent or system agent it notices why", () => {
  expect(agentsViewKey(at("hya-plan"), key("return"), rows)).toEqual({ type: "select", agent: "hya-plan" })
  expect(agentsViewKey(at("hya-scout"), key("return"), rows)).toEqual({ type: "update", view: { ...at("hya-scout"), notice: { tone: "info", text: "hya-scout is a subagent; only primary agents run a session" } } })
  expect(agentsViewKey(at("hya-compaction"), key("return"), rows)).toEqual({ type: "update", view: { ...at("hya-compaction"), notice: { tone: "info", text: "hya-compaction is a system agent; only primary agents run a session" } } })
})

test("m and t open the model and effort lists for any section, pinned agents included", () => {
  for (const agent of ["hya-main", "hya-scout", "hya-compaction"]) {
    expect(agentsViewKey(at(agent), key("m", { sequence: "m" }), rows)).toEqual({ type: "pickModel", agent })
    expect(agentsViewKey(at(agent), key("t", { sequence: "t" }), rows)).toEqual({ type: "pickEffort", agent })
  }
})

test("c clears a remembered model; a pinned agent names its file, no preference notices", () => {
  expect(agentsViewKey(at("hya-scout"), key("c", { sequence: "c" }), rows)).toEqual({ type: "clear", agent: "hya-scout" })
  expect(agentsViewKey(at("hya-main"), key("c", { sequence: "c" }), rows)).toEqual({ type: "update", view: { ...at("hya-main"), notice: { tone: "info", text: "hya-main's model is pinned in /c/config.yaml; m changes it" } } })
  expect(agentsViewKey(at("hya-plan"), key("c", { sequence: "c" }), rows)).toEqual({ type: "update", view: { ...at("hya-plan"), notice: { tone: "info", text: "hya-plan has no remembered preference" } } })
})

test("keys act only on a shown row: a filtered-out highlight is not selected", () => {
  const hidden: AgentsViewState = { agent: "hya-main", filter: "gpt", filtering: false }
  expect(agentsViewKey(hidden, key("return"), rows)).toEqual({ type: "update", view: { ...hidden, notice: { tone: "info", text: "No agent selected" } } })
})

test("r refreshes, Esc clears a filter first, then closes", () => {
  expect(agentsViewKey(at("hya-main"), key("r", { sequence: "r" }), rows)).toEqual({ type: "refresh" })
  expect(agentsViewKey({ ...at("hya-main"), filter: "x" }, key("escape"), rows)).toMatchObject({ type: "update", view: { filter: "" } })
  expect(agentsViewKey(at("hya-main"), key("escape"), rows)).toEqual({ type: "close" })
})

test("rows, header, and divider rules fit the width", () => {
  expect(agentLine(rows[0]!, 80)).toContain("anthropic/claude")
  expect(agentHeaderLine(80)).toContain("EFFECTIVE MODEL")
  for (const width of [20, 40, 74]) {
    expect(Bun.stringWidth(agentLine(rows[0]!, width))).toBeLessThanOrEqual(width)
    expect(Bun.stringWidth(sectionRule("Subagents", width))).toBe(width)
  }
  expect(sectionRule("Subagents", 20)).toBe("── Subagents ───────")
})

test("agentsViewHint reflects busy, filtering, and the key hints", () => {
  expect(agentsViewHint({ ...at("hya-main"), busy: { label: "Saving", startedAt: 0 } })).toContain("Esc cancels")
  expect(agentsViewHint({ ...at("hya-main"), filtering: true })).toContain("Type to filter")
  expect(agentsViewHint(at("hya-main"))).toBe("↑↓ move · Enter select · m model · t effort · c clear · r refresh · / filter · Esc close")
})

test("at about 80 terminal columns the effort column stays whole; wide screens widen the model column", () => {
  const pinned: AgentModelState = { ...rows[0]!, effective: { providerId: "12th", modelId: "MiniMaxAI/MiniMax-M2.5" }, effort: "high", effortSource: "AGENT_EFFORT_SOURCE_CONFIGURED" }
  expect(agentLine(pinned, 73)).toEndWith("high (config)")
  expect(agentLine(pinned, 120)).toContain("12th/MiniMaxAI/MiniMax-M2.5")
})
