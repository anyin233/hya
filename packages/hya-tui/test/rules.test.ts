import { expect, test } from "bun:test"
import type { SavedRule } from "../src/client"
import type { KeyLike } from "../src/keys/bindings"
import {
  initialRulesView,
  permissionText,
  ruleHeaderLine,
  ruleLine,
  ruleTimeText,
  rulesViewHint,
  rulesViewKey,
  settleRulesView,
  shownRules,
  type RulesViewState,
} from "../src/state/rules"

const key = (name: string, extra: Partial<KeyLike> = {}): KeyLike => ({
  name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra,
})

const rules: SavedRule[] = [
  { id: "r1", permission: "RULE_PERMISSION_ALLOW", tool: "bash", pattern: "git *", timeCreated: "2026-01-02T03:04:00Z" },
  { id: "r2", permission: "RULE_PERMISSION_DENY", tool: "edit", pattern: "/etc/*" },
]

test("permissionText maps every RulePermission", () => {
  expect(permissionText("RULE_PERMISSION_ALLOW")).toBe("allow")
  expect(permissionText("RULE_PERMISSION_ASK")).toBe("ask")
  expect(permissionText("RULE_PERMISSION_DENY")).toBe("deny")
  expect(permissionText(undefined)).toBe("—")
  expect(permissionText("RULE_PERMISSION_UNSPECIFIED")).toBe("—")
})

test("ruleTimeText reads a relative age, dashes when unset or invalid", () => {
  const now = new Date("2026-01-02T03:06:00Z").getTime()
  expect(ruleTimeText("2026-01-02T03:04:00Z", now)).toBe("2m ago")
  expect(ruleTimeText(undefined)).toBe("—")
  expect(ruleTimeText("not-a-date")).toBe("—")
})

test("ruleLine and ruleHeaderLine fit the given width and show a fallback wildcard", () => {
  const line = ruleLine(rules[0]!, 80)
  expect(line).toContain("allow")
  expect(line).toContain("bash")
  expect(line).toContain("git *")
  expect(line).toContain("ago")
  expect(ruleLine(rules[1]!, 80)).toContain("—") // no timeCreated
  expect(ruleHeaderLine(80)).toContain("EFFECT")
  expect(Bun.stringWidth(ruleLine(rules[0]!, 20))).toBeLessThanOrEqual(20)
})

test("ruleLine keeps a tool-wide grant's empty pattern distinct from an action-wide `*`", () => {
  const toolWide: SavedRule = { id: "r3", permission: "RULE_PERMISSION_ALLOW", tool: "read", pattern: "" }
  const actionWide: SavedRule = { id: "r4", permission: "RULE_PERMISSION_ALLOW", tool: "bash", pattern: "*" }
  expect(ruleLine(toolWide, 80)).not.toContain("*")
  expect(ruleLine(actionWide, 80)).toContain("*")
})

test("shownRules filters by tool, pattern, or effect", () => {
  expect(shownRules({ filter: "" }, rules)).toHaveLength(2)
  expect(shownRules({ filter: "bash" }, rules)).toEqual([rules[0]])
  expect(shownRules({ filter: "deny" }, rules)).toEqual([rules[1]])
  expect(shownRules({ filter: "etc" }, rules)).toEqual([rules[1]])
  expect(shownRules({ filter: "nope" }, rules)).toEqual([])
})

test("initialRulesView highlights the first row", () => {
  expect(initialRulesView(rules).rule).toBe("r1")
  expect(initialRulesView([]).rule).toBeUndefined()
})

test("settleRulesView keeps the highlight when the row still exists, else moves to the first", () => {
  const view: RulesViewState = { rule: "r2", filter: "", filtering: false }
  expect(settleRulesView(view, rules).rule).toBe("r2")
  expect(settleRulesView(view, [rules[0]!]).rule).toBe("r1")
  expect(settleRulesView(view, []).rule).toBe("r2")
})

test("Up/Down move the highlight with wrap-around", () => {
  const view: RulesViewState = { rule: "r1", filter: "", filtering: false }
  const down = rulesViewKey(view, key("down"), rules)
  expect(down).toEqual({ type: "update", view: { ...view, rule: "r2" } })
  const wrapped = rulesViewKey({ ...view, rule: "r2" }, key("down"), rules)
  expect(wrapped).toEqual({ type: "update", view: { ...view, rule: "r1" } })
})

test("d asks to confirm, Enter deletes, Esc backs out without deleting", () => {
  const view: RulesViewState = { rule: "r1", filter: "", filtering: false }
  const asked = rulesViewKey(view, key("d", { sequence: "d" }), rules)
  expect(asked).toEqual({ type: "update", view: { ...view, notice: undefined, confirm: "r1" } })
  const confirming = { ...view, confirm: "r1" }
  expect(rulesViewKey(confirming, key("return"), rules)).toEqual({ type: "delete", rule: "r1" })
  expect(rulesViewKey(confirming, key("escape"), rules)).toEqual({ type: "update", view: { ...confirming, confirm: undefined } })
})

test("d with no rule selected notices instead of confirming", () => {
  const view: RulesViewState = { rule: undefined, filter: "", filtering: false }
  const outcome = rulesViewKey(view, key("d", { sequence: "d" }), [])
  expect(outcome).toEqual({ type: "update", view: { ...view, notice: { tone: "info", text: "No rule selected" } } })
})

test("r refreshes, Esc closes the view", () => {
  const view: RulesViewState = { rule: "r1", filter: "", filtering: false }
  expect(rulesViewKey(view, key("r", { sequence: "r" }), rules)).toEqual({ type: "refresh" })
  expect(rulesViewKey(view, key("escape"), rules)).toEqual({ type: "close" })
})

test("filter mode: typing filters, Esc clears the filter first, Enter keeps it", () => {
  const view: RulesViewState = { rule: "r1", filter: "", filtering: true }
  const typed = rulesViewKey(view, key("b", { sequence: "b" }), rules)
  expect(typed).toEqual({ type: "update", view: settleRulesView({ ...view, filter: "b" }, rules) })
  const kept = rulesViewKey({ ...view, filter: "bash" }, key("return"), rules)
  expect(kept).toEqual({ type: "update", view: { ...view, filter: "bash", filtering: false } })
})

test("Esc while busy cancels the call instead of closing", () => {
  const view: RulesViewState = { rule: "r1", filter: "", filtering: false, busy: { kind: "refresh", label: "Refreshing", startedAt: 0 } }
  expect(rulesViewKey(view, key("escape"), rules)).toEqual({ type: "cancelBusy" })
  expect(rulesViewKey(view, key("d", { sequence: "d" }), rules)).toEqual({ type: "none" })
})

test("rulesViewHint reflects busy, confirm, filtering, and the default key hints", () => {
  expect(rulesViewHint({ rule: undefined, filter: "", filtering: false, busy: { kind: "delete", label: "Deleting", startedAt: 0 } })).toContain("Esc cancels")
  expect(rulesViewHint({ rule: "r1", filter: "", filtering: false, confirm: "r1" })).toBe("Enter deletes · Esc cancels")
  expect(rulesViewHint({ rule: "r1", filter: "", filtering: true })).toContain("Type to filter")
  expect(rulesViewHint({ rule: "r1", filter: "", filtering: false })).toContain("Esc close")
})
