import { afterEach, expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"

import {
  discoverPluginSpecs,
  parseAdapterOptions,
} from "../src/loader/discovery"

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("parses adapter plugin options", () => {
  expect(parseAdapterOptions(undefined).plugin).toEqual([])
  expect(
    parseAdapterOptions(
      '{"plugin":["./a.ts",["pkg@1.2.3",{"enabled":true,"limit":2}]]}',
    ).plugin,
  ).toEqual(["./a.ts", ["pkg@1.2.3", { enabled: true, limit: 2 }]])
})

test("rejects malformed adapter options", () => {
  expect(() => parseAdapterOptions('{"plugin":[42]}')).toThrow(
    "invalid OpenCode adapter options",
  )
})

test("discovers project and global OpenCode plugin files", async () => {
  const root = await makeTempDir()
  const directory = path.join(root, "project")
  const xdgConfigHome = path.join(root, "xdg")
  const localPlugin = path.join(directory, ".opencode", "plugin", "local.ts")
  const localPlugins = path.join(directory, ".opencode", "plugins", "extra.js")
  const globalPlugin = path.join(
    xdgConfigHome,
    "opencode",
    "plugins",
    "global.ts",
  )
  const ignored = path.join(directory, ".opencode", "plugins", "ignored.md")
  for (const file of [localPlugin, localPlugins, globalPlugin, ignored]) {
    await mkdir(path.dirname(file), { recursive: true })
    await writeFile(file, "export default {}")
  }

  const specs = await discoverPluginSpecs({
    directory,
    xdgConfigHome,
    home: path.join(root, "home"),
  })

  expect(specs).toEqual([
    pathToFileURL(globalPlugin).href,
    pathToFileURL(localPlugin).href,
    pathToFileURL(localPlugins).href,
  ])
})

test("discovers OpenCode config plugin entries relative to their config file", async () => {
  // Given: a project OpenCode config that declares local and npm plugins.
  const root = await makeTempDir()
  const directory = path.join(root, "project")
  const configFile = path.join(directory, ".opencode", "opencode.json")
  const pluginFile = path.join(directory, ".opencode", "configured.ts")
  await mkdir(path.dirname(configFile), { recursive: true })
  await writeFile(pluginFile, "export default {}")
  await writeFile(
    configFile,
    JSON.stringify({
      plugin: [["./configured.ts", { source: "config" }], "npm-plugin@1.0.0"],
    }),
  )

  // When: the adapter discovers OpenCode plugin specs for the project.
  const specs = await discoverPluginSpecs({
    directory,
    xdgConfigHome: path.join(root, "xdg"),
    home: path.join(root, "home"),
  })

  // Then: path-like specs are resolved using the declaring config file.
  expect(specs).toEqual([
    [pathToFileURL(pluginFile).href, { source: "config" }],
    "npm-plugin@1.0.0",
  ])
})

test("discovers OpenCode JSONC config plugin entries", async () => {
  // Given: a JSONC config with comments and trailing commas.
  const root = await makeTempDir()
  const directory = path.join(root, "project")
  const configFile = path.join(directory, ".opencode", "opencode.jsonc")
  const pluginFile = path.join(directory, ".opencode", "jsonc-plugin.ts")
  await mkdir(path.dirname(configFile), { recursive: true })
  await writeFile(pluginFile, "export default {}")
  await writeFile(
    configFile,
    `{
      // OpenCode documents opencode.jsonc as a supported config format.
      "plugin": [
        "./jsonc-plugin.ts",
      ],
    }`,
  )

  // When: the adapter discovers OpenCode plugin specs for the project.
  const specs = await discoverPluginSpecs({
    directory,
    xdgConfigHome: path.join(root, "xdg"),
    home: path.join(root, "home"),
  })

  // Then: JSONC plugin specs are parsed and path-resolved.
  expect(specs).toEqual([pathToFileURL(pluginFile).href])
})

test("deduplicates OpenCode config npm plugins by package name", async () => {
  // Given: global and local OpenCode configs declare the same npm plugin.
  const root = await makeTempDir()
  const directory = path.join(root, "project")
  const xdgConfigHome = path.join(root, "xdg")
  const globalConfig = path.join(xdgConfigHome, "opencode", "opencode.json")
  const localConfig = path.join(directory, ".opencode", "opencode.json")
  await mkdir(path.dirname(globalConfig), { recursive: true })
  await mkdir(path.dirname(localConfig), { recursive: true })
  await writeFile(
    globalConfig,
    JSON.stringify({
      plugin: ["shared-plugin@1.0.0", "global-only@1.0.0"],
    }),
  )
  await writeFile(
    localConfig,
    JSON.stringify({
      plugin: [["shared-plugin@2.0.0", { source: "local" }], "local-only@1.0.0"],
    }),
  )

  // When: the adapter discovers merged OpenCode plugin specs.
  const specs = await discoverPluginSpecs({
    directory,
    xdgConfigHome,
    home: path.join(root, "home"),
  })

  // Then: the local shared package wins while unrelated plugins remain ordered.
  expect(specs).toEqual([
    "global-only@1.0.0",
    ["shared-plugin@2.0.0", { source: "local" }],
    "local-only@1.0.0",
  ])
})

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  tempDirs.push(created)
  return created
}
