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
  { agentId: "build", mode: "primary", settable: false, configured: true, configurationPath: "/c/config.yaml", effective: { providerId: "anthropic", modelId: "claude" }, source: "AGENT_MODEL_SOURCE_CONFIGURED" },
  { agentId: "compaction", mode: "subagent", hidden: true, settable: true, effective: { providerId: "anthropic", modelId: "claude" }, source: "AGENT_MODEL_SOURCE_DEFAULT" },
  { agentId: "explore", mode: "subagent", settable: true, effective: { providerId: "openai", modelId: "gpt" }, source: "AGENT_MODEL_SOURCE_REMEMBERED", preference: { providerId: "openai", modelId: "gpt" } },
  { agentId: "plan", mode: "primary", settable: true, effective: { providerId: "google", modelId: "gemini" }, source: "AGENT_MODEL_SOURCE_DEFAULT", preference: {} },
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
    "— Primary agents", "build", "plan",
    "— Subagents", "explore",
    "— System agents", "compaction",
  ])
})

test("a filter drops sections left empty and matches section titles", () => {
  expect(agentsViewLines({ filter: "gpt" }, rows).map((line) => line.kind === "divider" ? line.title : line.row.agentId)).toEqual(["Subagents", "explore"])
  expect(shownAgents({ filter: "system" }, rows).map((row) => row.agentId)).toEqual(["compaction"])
})

test("Up/Down follow display order across sections and skip dividers", () => {
  const down = (view: AgentsViewState) => {
    const outcome = agentsViewKey(view, key("down"), rows)
    if (outcome.type !== "update") throw new Error(outcome.type)
    return outcome.view
  }
  const order: string[] = []
  let view = at("build")
  for (let i = 0; i < 4; i++) { view = down(view); order.push(view.agent!) }
  expect(order).toEqual(["plan", "explore", "compaction", "build"])
  const up = agentsViewKey(at("build"), key("up"), rows)
  expect(up.type === "update" && up.view.agent).toBe("compaction")
})

test("initialAgentsView opens on the session's agent, else the first primary agent", () => {
  expect(initialAgentsView(rows, "plan").agent).toBe("plan")
  expect(initialAgentsView(rows, "gone").agent).toBe("build")
  expect(initialAgentsView([]).agent).toBeUndefined()
})

test("settleAgentsView keeps the highlight when the row still exists", () => {
  expect(settleAgentsView(at("explore"), rows).agent).toBe("explore")
  expect(settleAgentsView(at("explore"), [rows[3]!]).agent).toBe("plan")
})

test("Enter selects a primary agent; on a subagent or system agent it notices why", () => {
  expect(agentsViewKey(at("plan"), key("return"), rows)).toEqual({ type: "select", agent: "plan" })
  expect(agentsViewKey(at("explore"), key("return"), rows)).toEqual({ type: "update", view: { ...at("explore"), notice: { tone: "info", text: "explore is a subagent; only primary agents run a session" } } })
  expect(agentsViewKey(at("compaction"), key("return"), rows)).toEqual({ type: "update", view: { ...at("compaction"), notice: { tone: "info", text: "compaction is a system agent; only primary agents run a session" } } })
})

test("m and t open the model and effort lists for any section, pinned agents included", () => {
  for (const agent of ["build", "explore", "compaction"]) {
    expect(agentsViewKey(at(agent), key("m", { sequence: "m" }), rows)).toEqual({ type: "pickModel", agent })
    expect(agentsViewKey(at(agent), key("t", { sequence: "t" }), rows)).toEqual({ type: "pickEffort", agent })
  }
})

test("c clears a remembered model; a pinned agent names its file, no preference notices", () => {
  expect(agentsViewKey(at("explore"), key("c", { sequence: "c" }), rows)).toEqual({ type: "clear", agent: "explore" })
  expect(agentsViewKey(at("build"), key("c", { sequence: "c" }), rows)).toEqual({ type: "update", view: { ...at("build"), notice: { tone: "info", text: "build's model is pinned in /c/config.yaml; m changes it" } } })
  expect(agentsViewKey(at("plan"), key("c", { sequence: "c" }), rows)).toEqual({ type: "update", view: { ...at("plan"), notice: { tone: "info", text: "plan has no remembered preference" } } })
})

test("keys act only on a shown row: a filtered-out highlight is not selected", () => {
  const hidden: AgentsViewState = { agent: "build", filter: "gpt", filtering: false }
  expect(agentsViewKey(hidden, key("return"), rows)).toEqual({ type: "update", view: { ...hidden, notice: { tone: "info", text: "No agent selected" } } })
})

test("r refreshes, Esc clears a filter first, then closes", () => {
  expect(agentsViewKey(at("build"), key("r", { sequence: "r" }), rows)).toEqual({ type: "refresh" })
  expect(agentsViewKey({ ...at("build"), filter: "x" }, key("escape"), rows)).toMatchObject({ type: "update", view: { filter: "" } })
  expect(agentsViewKey(at("build"), key("escape"), rows)).toEqual({ type: "close" })
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
  expect(agentsViewHint({ ...at("build"), busy: { label: "Saving", startedAt: 0 } })).toContain("Esc cancels")
  expect(agentsViewHint({ ...at("build"), filtering: true })).toContain("Type to filter")
  expect(agentsViewHint(at("build"))).toBe("↑↓ move · Enter select · m model · t effort · c clear · r refresh · / filter · Esc close")
})

test("at about 80 terminal columns the effort column stays whole; wide screens widen the model column", () => {
  const pinned: AgentModelState = { ...rows[0]!, effective: { providerId: "12th", modelId: "MiniMaxAI/MiniMax-M2.5" }, effort: "high", effortSource: "AGENT_EFFORT_SOURCE_CONFIGURED" }
  expect(agentLine(pinned, 73)).toEndWith("high (config)")
  expect(agentLine(pinned, 120)).toContain("12th/MiniMaxAI/MiniMax-M2.5")
})
