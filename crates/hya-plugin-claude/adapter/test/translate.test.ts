import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs, makePluginDir, makeTempDir } from "./helpers"
import { sanitizeToken, translatePlugin, TranslateError } from "../src/translate"

afterEach(cleanupTempDirs)

async function makeFullPlugin(): Promise<string> {
  return makePluginDir({
    name: "Code Review",
    version: "1.2.3",
    description: "Reviews code",
    agents: [{ name: "reviewer", body: "Review the diff.", tools: ["Read", "Bash"], model: "anthropic/claude-sonnet" }],
    skills: [{ name: "audit", body: "Audit a project." }],
    commands: [{ name: "review", body: "Run a review pass." }],
    mcpServers: {
      vecdb: { command: ["python3", "vecdb.py"], env: { TOKEN: "t" } },
      remote: { url: "https://example.com/mcp" },
    },
  })
}

describe("translatePlugin", () => {
  test("translates identity, agents, skills, commands, and mcp", async () => {
    const dir = await makeFullPlugin()
    const translation = translatePlugin(dir)

    expect(translation.namespace).toBe("code-review")
    expect(translation.identity).toEqual({
      id: "claude/code-review",
      version: "1.2.3",
      publisher: "claude",
    })
    const ids = translation.skills.map((skill) => skill.id)
    // Claude agents remain agents; skills and commands become skill resources.
    expect(ids).toEqual(["audit", "review"])
    for (const skill of translation.skills) {
      expect(skill.digest).toMatch(/^[0-9a-f]{64}$/)
      expect(skill.path).toBe(`skills/${skill.id}.md`)
    }
    expect(translation.mcp.map((entry) => entry.id)).toEqual(["remote", "vecdb"])
    const vecdb = translation.mcp.find((entry) => entry.id === "vecdb")
    expect(vecdb?.path).toBe("mcp/vecdb.json")
    expect(vecdb?.content).toContain("vecdb.py")

    expect(translation.agents).toHaveLength(1)
    expect(translation.agents[0]?.id).toBe("reviewer")
    expect(translation.agents[0]?.promptPath).toBe("prompts/reviewer.md")
    expect(translation.agents[0]?.prompt).toContain("Review the diff.")
    expect(translation.agents[0]?.tools).toEqual(["harness:tool/read", "harness:tool/bash"])
  })

  test("emits every manifest-referenced file and a sorted manifest", async () => {
    const dir = await makeFullPlugin()
    const translation = translatePlugin(dir)
    const paths = translation.files.map((file) => file.path).sort()
    const referenced = [...translation.manifestYaml.matchAll(/path: "(.+?)"/g)]
      .map((match) => match[1])
      .filter((entry): entry is string => entry !== undefined)
    for (const entry of referenced) {
      expect(paths).toContain(entry)
    }

    const manifest = translation.manifestYaml
    expect(manifest).toContain("kind: AgentSetBundle")
    expect(manifest).toContain(`id: "claude/code-review"`)
    expect(manifest).toContain('publisher: "claude"')
    expect(manifest).toContain('namespace: "code-review"')
    expect(manifest).toContain("role: subagent")
    expect(manifest).not.toContain("spawn_lifecycle")
    expect(manifest).toContain(`prompt: "prompts/reviewer.md"`)
    expect(manifest).toContain('model: "anthropic/claude-sonnet"')
    expect(manifest).toContain('"harness:tool/read"')
    expect(manifest).toContain("process:")
    expect(manifest).toContain("kind: claude")
  })

  test("emits an agentless Plugin when agents/ is absent", async () => {
    const dir = await makePluginDir({ name: "bare", description: "Bare plugin" })
    const translation = translatePlugin(dir)
    expect(translation.agents).toHaveLength(0)
    expect(translation.skills).toHaveLength(0)
    expect(translation.manifestYaml).toContain("skills:")
    expect(translation.manifestYaml).toContain("[]")
    expect(translation.manifestYaml).toContain("kind: Plugin")
    expect(translation.manifestYaml).not.toContain("\nagent:")
    expect(translation.manifestYaml).not.toContain("\nagents:")
  })

  test("preserves every declared Claude agent in an AgentSetBundle", async () => {
    const dir = await makePluginDir({
      name: "team",
      agents: [
        { name: "reviewer", body: "Review changes." },
        { name: "tester", body: "Test changes." },
      ],
    })
    const translation = translatePlugin(dir)
    expect(translation.agents.map((agent) => agent.id)).toEqual(["reviewer", "tester"])
    expect(translation.manifestYaml.match(/role: subagent/g)).toHaveLength(2)
    expect(translation.manifestYaml).not.toContain("role: main")
  })

  test("reads the nested .claude-plugin layout and dedupes colliding ids", async () => {
    const dir = await makePluginDir({
      name: "collide",
      nested: true,
      skills: [{ name: "tool", body: "Skill body" }],
      commands: [{ name: "tool", body: "Command body" }],
    })
    const translation = translatePlugin(dir)
    expect(translation.namespace).toBe("collide")
    expect(translation.skills.map((skill) => skill.id)).toEqual(["tool", "tool-2"])
  })

  test("rejects directories without plugin.json", async () => {
    const dir = await makeTempDir()
    expect(() => translatePlugin(dir)).toThrow(TranslateError)
  })

  test("rejects hook forms that cannot be represented instead of dropping them", async () => {
    const dir = await makePluginDir({
      hooksJson: {
        PreToolUse: [{ hooks: [{ type: "prompt", prompt: "Decide" }] }],
      },
    })
    expect(() => translatePlugin(dir)).toThrow("unsupported non-command hook")
    const promptVeto = await makePluginDir({
      hooksJson: { UserPromptSubmit: [{ hooks: [{ type: "command", command: "true" }] }] },
    })
    expect(() => translatePlugin(promptVeto)).toThrow("UserPromptSubmit")
    const stop = await makePluginDir({
      hooksJson: { Stop: [{ hooks: [{ type: "command", command: "true" }] }] },
    })
    expect(() => translatePlugin(stop)).toThrow("Stop")
  })

  test("accepts the documented top-level hooks wrapper", async () => {
    const dir = await makePluginDir({
      hooksJson: {
        hooks: {
          PreToolUse: [
            { matcher: "Read", hooks: [{ type: "command", command: "true" }] },
          ],
        },
      },
    })
    const translation = translatePlugin(dir)
    expect(translation.hookGroups["tool.execute.before"]).toHaveLength(1)
  })
})

describe("sanitizeToken", () => {
  test("mirrors the Rust namespace sanitizer", () => {
    expect(sanitizeToken("Code Review")).toBe("code-review")
    expect(sanitizeToken("My__Plugin")).toBe("my-plugin")
    expect(sanitizeToken("--v2--")).toBe("v2")
    expect(sanitizeToken("工具")).toBe("")
  })
})
