import { expect, test } from "bun:test"
import { compactLine, skillGroup, startupLines, startupSectionIds, type StartupInput, type StartupOptions } from "../src/state/startup"

const env = { HOME: "/home/u" }
const compact: StartupOptions = { resources: "compact", sections: [...startupSectionIds], quiet: false }

const input: StartupInput = {
  notices: [],
  skills: [
    { name: "debug", source: "builtin", location: "/home/u/.codex/skills/debug/SKILL.md" },
    { name: "brainstorm", source: "builtin", location: "<built-in>" },
    { name: "review", source: "project", location: "/w/.hya/skills/review/SKILL.md" },
  ],
  commands: [{ name: "release", source: "command" }, { name: "debug", source: "skill" }, { name: "fix" }],
  mcp: [
    { name: "files", state: "MCP_SERVER_STATE_CONNECTED", tools: ["mcp__files__read"] },
    { name: "slack", state: "MCP_SERVER_STATE_FAILED", error: "connection closed" },
    { name: "linear", state: "MCP_SERVER_STATE_DISCONNECTED", authRequired: true },
    { name: "late", state: "MCP_SERVER_STATE_DESIRED" },
  ],
  bundles: [
    { id: "hya/base-tools", scope: "first_party", state: "active", version: "1.0.0" },
    { id: "hya/code-mode", scope: "first_party", state: "disabled", enabled: false },
    { id: "me/broken", scope: "user", state: "unreadable", error: "bad yaml" },
  ],
  extensions: [
    { id: "hya/basic-tui-components", version: "1", state: "running", isolated: false, jit: true, permissions: [], warnings: [], log: [] },
    { id: "me/panel", version: "1", state: "failed", reason: "host exited", isolated: false, jit: false, permissions: [], warnings: [], log: [] },
  ],
}

test("compact: one line per section, sorted, with problems after it in the warning tone", () => {
  const lines = startupLines(input, compact, 200, env)
  expect(lines[0]!.label).toBe("[Skills]")
  expect(lines.map((line) => `${line.tone}: ${line.text}`)).toEqual([
    "muted: [Skills] brainstorm, debug, review",
    "muted: [Slash commands] /fix, /release",
    "muted: [MCP] files, late (connecting), linear (disconnected), slack",
    "warning:   slack: failed: connection closed · /mcp to fix",
    "warning:   linear: needs login · /mcp to connect",
    "muted: [Bundles] hya/base-tools, hya/code-mode (disabled)",
    "warning:   me/broken: unreadable: bad yaml · /bundles",
    "muted: [Extensions] hya/basic-tui-components",
    "warning:   me/panel: failed: host exited · /extensions",
  ])
})

test("quiet keeps only the problems; config notices always come first", () => {
  const lines = startupLines({ ...input, notices: ["[Config] ~/.config/hya/tui.toml · 1 warning · /config lists them", "  Unknown key x; ignored"] }, { ...compact, quiet: true }, 200, env)
  expect(lines.map((line) => line.text)).toEqual([
    "[Config] ~/.config/hya/tui.toml · 1 warning · /config lists them",
    "  Unknown key x; ignored",
    "[MCP] slack: failed: connection closed · /mcp to fix",
    "[MCP] linear: needs login · /mcp to connect",
    "[Bundles] me/broken: unreadable: bad yaml · /bundles",
    "[Extensions] me/panel: failed: host exited · /extensions",
  ])
  expect(lines.every((line) => line.tone === "warning")).toBe(true)
})

test("sections without data are left out; sections follow the configured list", () => {
  const lines = startupLines({ notices: [], skills: input.skills }, { ...compact, sections: ["mcp", "skills"] }, 200, env)
  expect(lines.map((line) => line.text)).toEqual(["[Skills] brainstorm, debug, review"])
  expect(startupLines({ notices: [], contextFiles: [{ path: "/home/u/AGENTS.md", scope: "user", bytes: 120 }] }, compact, 200, env)[0]!.text).toBe("[Context] ~/AGENTS.md")
  // An empty section has no compact line; the full list says none.
  expect(startupLines({ notices: [], mcp: [] }, compact, 200, env)).toEqual([])
  expect(startupLines({ notices: [], mcp: [] }, { ...compact, resources: "full" }, 200, env).map((line) => line.text)).toEqual(["[MCP]", "  none"])
})

test("full groups by source with details; skills group by their directory", () => {
  expect(skillGroup(input.skills![0]!, env)).toBe("user ~/.codex/skills")
  expect(skillGroup(input.skills![1]!, env)).toBe("builtin")
  expect(skillGroup(input.skills![2]!, env)).toBe("project /w/.hya/skills")
  const lines = startupLines({ notices: [], skills: input.skills, bundles: input.bundles }, { ...compact, resources: "full" }, 200, env).map((line) => line.text)
  expect(lines).toEqual([
    "[Skills]",
    "  builtin: brainstorm",
    "  user ~/.codex/skills: debug",
    "  project /w/.hya/skills: review",
    "[Bundles]",
    "  first_party",
    "    hya/base-tools · 1.0.0 · active",
    "    hya/code-mode · disabled",
    "  me/broken: unreadable: bad yaml · /bundles",
  ])
})

test("a compact line ends in +N when the width runs out", () => {
  const names = ["alpha", "bravo", "charlie", "delta", "echo"]
  expect(compactLine("[Skills]", names, 200)).toBe("[Skills] alpha, bravo, charlie, delta, echo")
  expect(compactLine("[Skills]", names, 30)).toBe("[Skills] alpha, bravo, +3")
  expect(compactLine("[Skills]", names, 12)).toBe("[Skills] +5")
  expect(compactLine("[Skills]", [], 30)).toBe("[Skills] none")
})

test("skill and command diagnostics and commands hidden by the TUI's own are problems", () => {
  const lines = startupLines({
    notices: [],
    skills: [{ name: "guide", source: "bundle", location: "bundle:me/kit/skills/guide/SKILL.md", bundleId: "me/kit" }],
    skillDiagnostics: [{ path: "/home/u/.codex/skills/x/SKILL.md", kind: "missing_description", message: "/home/u/.codex/skills/x/SKILL.md: no `description`; skipped" }],
    commands: [{ name: "status", source: "command", scope: "project" }],
    commandDiagnostics: [{ path: "/w/.hya/commands/skill:x.md", kind: "reserved", message: "/w/.hya/commands/skill:x.md: `/skill:x` is in the reserved skill namespace; skipped" }],
    shadowedCommands: [{ name: "status", source: "command", scope: "project" }],
  }, { ...compact, sections: ["skills", "commands"] }, 200, env)
  expect(lines.map((line) => `${line.tone}: ${line.text}`)).toEqual([
    "muted: [Skills] guide",
    "warning:   ~/.codex/skills/x/SKILL.md: no `description`; skipped · /skills",
    "muted: [Slash commands] /status",
    "warning:   /w/.hya/commands/skill:x.md: `/skill:x` is in the reserved skill namespace; skipped",
    "warning:   /status: the TUI's own /status hides this project command; rename its file to run it",
  ])
  expect(skillGroup({ name: "guide", source: "bundle", location: "bundle:me/kit/skills/guide/SKILL.md", bundleId: "me/kit" }, env)).toBe("bundle me/kit")
})
