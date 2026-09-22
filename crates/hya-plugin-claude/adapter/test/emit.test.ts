import { afterEach, describe, expect, test } from "bun:test"

import {
  cleanupTempDirs,
  makePluginDir,
  runEmitManifest,
} from "./helpers"

afterEach(cleanupTempDirs)

describe("--emit-bundle-manifest", () => {
  test("prints one JSON envelope whose manifest references only emitted files", async () => {
    const dir = await makePluginDir({
      name: "emit-plugin",
      version: "4.5.6",
      agents: [{ name: "lead", body: "Lead the plugin." }],
      skills: [{ name: "scan", body: "Scan things." }],
      mcpServers: { db: { command: ["db"] } },
    })
    const run = await runEmitManifest(dir)
    expect(run.exitCode).toBe(0)
    const envelope = JSON.parse(run.stdout) as { manifest: string; files: { path: string; content: string }[] }
    expect(envelope.manifest).toContain("kind: AgentSetBundle")
    expect(envelope.manifest).toContain('id: "claude/emit-plugin"')
    expect(envelope.manifest).toContain('namespace: "emit-plugin"')

    const referenced = [...envelope.manifest.matchAll(/path: "(.+?)"/g)].map((match) => match[1])
    const emitted = new Set(envelope.files.map((file) => file.path))
    for (const path of referenced) {
      expect(emitted.has(path)).toBe(true)
    }
  })

  test("emits an agentless Plugin for resource-only Claude plugins", async () => {
    const dir = await makePluginDir({
      name: "resource-only",
      skills: [{ name: "scan", body: "Scan things." }],
    })
    const run = await runEmitManifest(dir)
    expect(run.exitCode).toBe(0)
    const envelope = JSON.parse(run.stdout) as { manifest: string }
    expect(envelope.manifest).toContain("kind: Plugin")
    expect(envelope.manifest).not.toContain("\nagent:")
    expect(envelope.manifest).not.toContain("\nagents:")
  })

  test("fails with a diagnostic for a non-plugin directory", async () => {
    const run = await runEmitManifest("/tmp/definitely-not-a-plugin-dir-12345")
    expect(run.exitCode).toBe(1)
    expect(run.stderr).toContain("plugin.json")
  })
})
