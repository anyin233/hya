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

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  tempDirs.push(created)
  return created
}
