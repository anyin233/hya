import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs, makePluginDir, makeTempDir } from "./helpers"
import { sanitizeToken, translatePlugin, TranslateError } from "../src/translate"

afterEach(cleanupTempDirs)

async function makeFullPlugin(): Promise<string> {
  return makePluginDir({
    name: "Code Review",
    version: "1.2.3",
    description: "Reviews code",
    agents: [{ name: "reviewer", body: "Review the diff." }],
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
    // agents + skills + commands all become skill resources with digests,
    // collected in agents/, skills/, commands/ order.
    expect(ids).toEqual(["reviewer", "audit", "review"])
    for (const skill of translation.skills) {
      expect(skill.digest).toMatch(/^[0-9a-f]{64}$/)
      expect(skill.path).toBe(`skills/${skill.id}.md`)
    }
    expect(translation.skills.find((skill) => skill.id === "reviewer")?.content).toContain("Review the diff.")

    expect(translation.mcp.map((entry) => entry.id)).toEqual(["remote", "vecdb"])
    const vecdb = translation.mcp.find((entry) => entry.id === "vecdb")
    expect(vecdb?.path).toBe("mcp/vecdb.json")
    expect(vecdb?.content).toContain("vecdb.py")

    // The bundle Agent is the first agents/*.md entry.
    expect(translation.agent.id).toBe("reviewer")
    expect(translation.agent.promptPath).toBe("prompts/reviewer.md")
    expect(translation.agent.prompt).toContain("Review the diff.")
  })

  test("emits every manifest-referenced file and a sorted manifest", async () => {
    const dir = await makeFullPlugin()
    const translation = translatePlugin(dir)
    const paths = translation.files.map((file) => file.path).sort()
    const referenced = [
      ...translation.skills.map((skill) => skill.path),
      ...translation.mcp.map((entry) => entry.path),
      translation.agent.promptPath,
    ].sort()
    expect(paths).toEqual(referenced)

    const manifest = translation.manifestYaml
    expect(manifest).toContain("kind: AgentBundle")
    expect(manifest).toContain(`id: "claude/code-review"`)
    expect(manifest).toContain('publisher: "claude"')
    expect(manifest).toContain('namespace: "code-review"')
    expect(manifest).toContain("role: main")
    expect(manifest).toContain("spawn_lifecycle: transient")
    expect(manifest).toContain(`prompt: "prompts/reviewer.md"`)
  })

  test("synthesizes the agent when agents/ is absent", async () => {
    const dir = await makePluginDir({ name: "bare", description: "Bare plugin" })
    const translation = translatePlugin(dir)
    expect(translation.agent.id).toBe("bare")
    expect(translation.agent.promptPath).toBe("prompts/claude-plugin.md")
    expect(translation.agent.prompt).toContain("bare")
    expect(translation.skills).toHaveLength(0)
    expect(translation.manifestYaml).toContain("skills:")
    expect(translation.manifestYaml).toContain("[]")
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
})

describe("sanitizeToken", () => {
  test("mirrors the Rust namespace sanitizer", () => {
    expect(sanitizeToken("Code Review")).toBe("code-review")
    expect(sanitizeToken("My__Plugin")).toBe("my-plugin")
    expect(sanitizeToken("--v2--")).toBe("v2")
    expect(sanitizeToken("工具")).toBe("")
  })
})
