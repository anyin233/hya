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

test("discovers COMPAT_CONFIG file plugins before project directories", async () => {
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

test("discovers COMPAT_CONFIG_CONTENT plugins after config directories", async () => {
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

test("resolves COMPAT_CONFIG_CONTENT path plugins relative to the project directory", async () => {
  const root = await makeTempDir()
  const directory = path.join(root, "project")
  const inlinePlugin = path.join(directory, "inline-plugin.ts")
  await mkdir(directory, { recursive: true })
  await writeFile(inlinePlugin, "export default {}")

  const specs = await discoverPluginSpecs({
    directory,
    worktree: directory,
    inlineConfig: JSON.stringify({
      plugin: [["./inline-plugin.ts", { source: "inline" }]],
    }),
    xdgConfigHome: path.join(root, "xdg"),
    home: path.join(root, "home"),
  })

  expect(specs).toEqual([
    [pathToFileURL(inlinePlugin).href, { source: "inline" }],
  ])
})

test("discovers project compat config files before config directories", async () => {
  const root = await makeTempDir()
  const worktree = path.join(root, "project")
  const directory = path.join(worktree, "nested")
  const parentConfig = path.join(worktree, "opencode.json")
  const childConfig = path.join(directory, "opencode.jsonc")
  const childPlugin = path.join(directory, "direct-plugin.ts")
  const dirConfig = path.join(directory, ".opencode", "opencode.json")
  await mkdir(directory, { recursive: true })
  await mkdir(path.dirname(dirConfig), { recursive: true })
  await writeFile(childPlugin, "export default {}")
  await writeFile(
    parentConfig,
    JSON.stringify({
      plugin: ["parent-file-plugin", "shared-plugin@1.0.0"],
    }),
  )
  await writeFile(
    childConfig,
    JSON.stringify({
      plugin: [
        ["./direct-plugin.ts", { source: "direct" }],
        "shared-plugin@2.0.0",
      ],
    }),
  )
  await writeFile(dirConfig, JSON.stringify({ plugin: ["dir-plugin"] }))

  const specs = await discoverPluginSpecs({
    directory,
    worktree,
    xdgConfigHome: path.join(root, "xdg"),
    home: path.join(root, "home"),
  })

  expect(specs).toEqual([
    "parent-file-plugin",
    [pathToFileURL(childPlugin).href, { source: "direct" }],
    "shared-plugin@2.0.0",
    "dir-plugin",
  ])
})

test("ignores config.json outside the global Compat config directory", async () => {
  const root = await makeTempDir()
  const directory = path.join(root, "project")
  const xdgConfigHome = path.join(root, "xdg")
  const globalConfig = path.join(xdgConfigHome, "compat", "config.json")
  const projectConfigJson = path.join(directory, ".opencode", "config.json")
  const projectCompatJson = path.join(directory, ".opencode", "opencode.json")
  await mkdir(path.dirname(globalConfig), { recursive: true })
  await mkdir(path.dirname(projectConfigJson), { recursive: true })
  await writeFile(globalConfig, JSON.stringify({ plugin: ["global-config-json"] }))
  await writeFile(
    projectConfigJson,
    JSON.stringify({ plugin: ["ignored-project-config-json"] }),
  )
  await writeFile(
    projectCompatJson,
    JSON.stringify({ plugin: ["project-compat-json"] }),
  )

  const specs = await discoverPluginSpecs({
    directory,
    worktree: directory,
    xdgConfigHome,
    home: path.join(root, "home"),
  })

  expect(specs).toEqual(["global-config-json", "project-compat-json"])
})

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "hya-compat-"))
  tempDirs.push(created)
  return created
}
