import fs from "node:fs"
import path from "node:path"

import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs, makePluginDir, makeTempDir, writePluginDir } from "./helpers"
import { discoverPluginSources, readPluginJson } from "../src/discovery"

afterEach(cleanupTempDirs)

describe("readPluginJson", () => {
  test("reads the flat plugin.json layout", async () => {
    const dir = await makePluginDir({ name: "flat", version: "2.1.0", description: "Flat" })
    const source = readPluginJson(dir)
    expect(source).not.toBeUndefined()
    expect(source?.manifest.name).toBe("flat")
    expect(source?.manifest.version).toBe("2.1.0")
    expect(source?.manifest.description).toBe("Flat")
    expect(source?.manifestPath).toBe("plugin.json")
  })

  test("reads the .claude-plugin/ layout and defaults the version", async () => {
    const dir = await makePluginDir({ name: "nested", nested: true })
    const source = readPluginJson(dir)
    expect(source).not.toBeUndefined()
    expect(source?.manifestPath).toBe(path.join(".claude-plugin", "plugin.json"))
    expect(source?.manifest.version).toBe("0.0.0")
  })

  test("returns undefined without a manifest and throws on invalid JSON", async () => {
    const empty = await makeTempDir()
    expect(readPluginJson(empty)).toBeUndefined()
    const broken = await makeTempDir()
    fs.writeFileSync(path.join(broken, "plugin.json"), "{not json")
    expect(() => readPluginJson(broken)).toThrow()
  })
})

describe("discoverPluginSources", () => {
  test("scans project and user plugin roots one level deep", async () => {
    const project = await makeTempDir()
    const home = await makeTempDir()
    await makePluginDir({ name: "proj-a" })
    // Rebuild fixtures inside the discovery roots instead of the default temp dir.
    const projectPlugin = path.join(project, ".claude/plugins/alpha")
    fs.mkdirSync(projectPlugin, { recursive: true })
    fs.writeFileSync(path.join(projectPlugin, "plugin.json"), `{"name": "alpha"}`)
    const userPlugin = path.join(home, ".claude/plugins/beta")
    fs.mkdirSync(path.join(userPlugin, ".claude-plugin"), { recursive: true })
    fs.writeFileSync(
      path.join(userPlugin, ".claude-plugin", "plugin.json"),
      `{"name": "beta", "version": "3.0.0"}`,
    )

    const found = discoverPluginSources({ cwd: project, home })
    expect(found.map((source) => source.manifest.name)).toEqual(["alpha", "beta"])
  })

  test("skips directories without manifests and invalid manifests", async () => {
    const project = await makeTempDir()
    const root = path.join(project, ".claude/plugins")
    fs.mkdirSync(path.join(root, "has-manifest"), { recursive: true })
    fs.writeFileSync(path.join(root, "has-manifest", "plugin.json"), `{"name": "good"}`)
    fs.mkdirSync(path.join(root, "no-manifest"), { recursive: true })
    fs.mkdirSync(path.join(root, "bad-manifest"), { recursive: true })
    fs.writeFileSync(path.join(root, "bad-manifest", "plugin.json"), `{"name": 42}`)

    const found = discoverPluginSources({ cwd: project, home: project })
    expect(found.map((source) => source.manifest.name)).toEqual(["good"])
  })

  test("discovers a project-local .claude-plugin source", async () => {
    const project = await makeTempDir()
    await writePluginDir(project, { name: "project-plugin", nested: true })
    expect(discoverPluginSources({ cwd: project, home: await makeTempDir() }).map((source) => source.manifest.name)).toEqual(["project-plugin"])
  })
})
