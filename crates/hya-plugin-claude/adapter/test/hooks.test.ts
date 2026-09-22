import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs } from "./helpers"
import {
  claudeHookPayload,
  claudeToolName,
  firstJsonObject,
  hookRegistrationsFrom,
  matcherMatches,
  parseClaudeHooks,
  toolDecisionFromClaudeStdout,
} from "../src/hooks"

afterEach(cleanupTempDirs)

const HOOKS_JSON = {
  PreToolUse: [
    { matcher: "Bash", hooks: [{ type: "command", command: "echo block", timeout: 5 }] },
    { matcher: "", hooks: [{ type: "command", command: "echo all" }] },
  ],
  PostToolUse: [
    { matcher: "*", hooks: [{ type: "command", command: "tee /tmp/seen" }] },
  ],
  UserPromptSubmit: [
    { hooks: [{ type: "prompt", command: "not-a-command-hook" }] },
  ],
  SessionStart: [{ hooks: [{ type: "command", command: "date" }] }],
  UnknownEvent: [{ hooks: [{ type: "command", command: "nope" }] }],
}

describe("parseClaudeHooks", () => {
  test("maps CC events to hya hook names and drops non-command entries", () => {
    const hooks = parseClaudeHooks(HOOKS_JSON)
    const registrations = hookRegistrationsFrom(hooks).map((entry) => entry.name)
    expect(registrations).toEqual(["session.start", "tool.execute.after", "tool.execute.before"])
    expect(hooks.groups["tool.execute.before"]).toHaveLength(2)
    expect(hooks.groups["tool.execute.before"]?.[0]?.commands[0]?.timeoutSeconds).toBe(5)
    expect(hooks.groups["message.user.before"]).toHaveLength(0)
    expect(hooks.groups["session.start"]).toHaveLength(1)
  })

  test("accepts the documented top-level hooks wrapper", () => {
    const hooks = parseClaudeHooks({
      hooks: {
        PreToolUse: [{ matcher: "Read", hooks: [{ type: "command", command: "true" }] }],
      },
    })
    expect(hookRegistrationsFrom(hooks)).toEqual([{ name: "tool.execute.before" }])
    expect(hooks.groups["tool.execute.before"]?.[0]?.matcher).toBe("Read")
  })

  test("tolerates malformed documents", () => {
    for (const document of [undefined, null, "nope", {}, { PreToolUse: "nope" }]) {
      expect(hookRegistrationsFrom(parseClaudeHooks(document))).toEqual([])
    }
  })
})

describe("matcherMatches", () => {
  test("empty, wildcard, pipe-separated, and exact matching", () => {
    expect(matcherMatches("", "Bash")).toBe(true)
    expect(matcherMatches("*", "Bash")).toBe(true)
    expect(matcherMatches("Bash|Edit", "Edit")).toBe(true)
    expect(matcherMatches("Bash|Edit", "Read")).toBe(false)
    expect(matcherMatches("Read", "Read")).toBe(true)
  })
})

test("claudeToolName maps only canonical builtins", () => {
  expect(claudeToolName("read")).toBe("Read")
  expect(claudeToolName("bash")).toBe("Bash")
  expect(claudeToolName("mcp__db__read")).toBe("mcp__db__read")
  expect(claudeToolName("customTool")).toBe("customTool")
})

describe("claudeHookPayload", () => {
  test("builds the CC stdin contract for tool events", () => {
    const payload = claudeHookPayload("tool.execute.before", {
      session: "s1",
      tool: "Bash",
      input: { command: "ls" },
    })
    expect(payload["hook_event_name"]).toBe("PreToolUse")
    expect(payload["session_id"]).toBe("s1")
    expect(payload["tool_name"]).toBe("Bash")
    expect(payload["tool_input"]).toEqual({ command: "ls" })
    expect(payload["cwd"]).toBe(process.cwd())
  })
})

describe("toolDecisionFromClaudeStdout", () => {
  test("translates block, deny, approve, and non-JSON output", () => {
    expect(toolDecisionFromClaudeStdout('{"decision":"block","reason":"no"}')).toEqual({
      outcome: "veto",
      reason: "no",
    })
    expect(
      toolDecisionFromClaudeStdout('{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"r"}}'),
    ).toEqual({ outcome: "veto", reason: "r" })
    expect(toolDecisionFromClaudeStdout('{"permissionDecision":"allow"}')).toEqual({
      outcome: "continue",
    })
    expect(toolDecisionFromClaudeStdout("plain text")).toBeUndefined()
    expect(toolDecisionFromClaudeStdout("{}")).toBeUndefined()
  })
})

describe("firstJsonObject", () => {
  test("finds JSON mixed into surrounding output", () => {
    expect(firstJsonObject('noise {"decision":"block"} tail')).toEqual({ decision: "block" })
    expect(firstJsonObject("no json here")).toBeUndefined()
  })
})
