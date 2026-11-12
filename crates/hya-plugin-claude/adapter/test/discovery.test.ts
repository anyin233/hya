import fs from "node:fs"
import path from "node:path"

import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs, makePluginDir, makeTempDir } from "./helpers"
import { readPluginJson } from "../src/discovery"

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
