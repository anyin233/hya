import { afterEach, expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"

import { discoverPluginSpecs } from "../src/loader/discovery"

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("discovers OPENCODE_CONFIG file plugins before project directories", async () => {
  const root = await makeTempDir()
  const directory = path.join(root, "project")
  const customConfigFile = path.join(root, "custom", "opencode.json")
  const customPlugin = path.join(root, "custom", "custom-plugin.ts")
  const projectConfig = path.join(directory, ".opencode", "opencode.json")
  await mkdir(path.dirname(customConfigFile), { recursive: true })
  await mkdir(path.dirname(projectConfig), { recursive: true })
  await writeFile(customPlugin, "export default {}")
  await writeFile(
    customConfigFile,
    JSON.stringify({
      plugin: [["./custom-plugin.ts", { source: "custom" }], "shared-plugin@1.0.0"],
    }),
  )
  await writeFile(
    projectConfig,
    JSON.stringify({
      plugin: ["project-plugin", "shared-plugin@2.0.0"],
    }),
  )

  const specs = await discoverPluginSpecs({
    directory,
    worktree: directory,
    customConfigFile,
    xdgConfigHome: path.join(root, "xdg"),
    home: path.join(root, "home"),
  })

  expect(specs).toEqual([
    [pathToFileURL(customPlugin).href, { source: "custom" }],
    "project-plugin",
    "shared-plugin@2.0.0",
  ])
})

test("discovers OPENCODE_CONFIG_CONTENT plugins after config directories", async () => {
  const root = await makeTempDir()
  const directory = path.join(root, "project")
  const projectConfig = path.join(directory, ".opencode", "opencode.json")
  await mkdir(path.dirname(projectConfig), { recursive: true })
  await writeFile(
    projectConfig,
    JSON.stringify({
      plugin: ["shared-plugin@1.0.0", "project-plugin"],
    }),
  )

  const specs = await discoverPluginSpecs({
    directory,
    worktree: directory,
    inlineConfig: JSON.stringify({
      plugin: [["shared-plugin@2.0.0", { source: "inline" }], "inline-plugin"],
    }),
    xdgConfigHome: path.join(root, "xdg"),
    home: path.join(root, "home"),
  })

  expect(specs).toEqual([
    "project-plugin",
    ["shared-plugin@2.0.0", { source: "inline" }],
    "inline-plugin",
  ])
})

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  tempDirs.push(created)
  return created
}
