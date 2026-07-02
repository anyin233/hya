import { afterEach, expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"

import {
  detectServerModuleShape,
  resolveLocalPluginSpec,
} from "../src/loader/shape"

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("resolves local plugin specs relative to their config file", async () => {
  const root = await makeTempDir()
  const configFile = path.join(root, ".opencode", "opencode.json")
  const pluginFile = path.join(root, ".opencode", "plugins", "local.ts")
  await mkdir(path.dirname(pluginFile), { recursive: true })
  await writeFile(pluginFile, "export default {}")

  const resolved = await resolveLocalPluginSpec(
    ["./plugins/local.ts", { enabled: true }],
    configFile,
  )

  expect(resolved).toEqual([
    pathToFileURL(pluginFile).href,
    { enabled: true },
  ])
})

test("resolves local plugin directories to index files", async () => {
  const root = await makeTempDir()
  const configFile = path.join(root, ".opencode", "opencode.json")
  const indexFile = path.join(root, ".opencode", "my-plugin", "index.ts")
  await mkdir(path.dirname(indexFile), { recursive: true })
  await writeFile(indexFile, "export default {}")

  const resolved = await resolveLocalPluginSpec("./my-plugin", configFile)

  expect(resolved).toBe(pathToFileURL(indexFile).href)
})

test("keeps npm plugin specs unchanged", async () => {
  await expect(
    resolveLocalPluginSpec(["some-plugin@1.2.3", { enabled: false }], "/tmp/config.json"),
  ).resolves.toEqual(["some-plugin@1.2.3", { enabled: false }])
})

test("detects supported Compat server module shapes", () => {
  const server = () => ({})

  expect(
    detectServerModuleShape({ default: { id: "v1", server } }).kind,
  ).toBe("v1_server")
  expect(detectServerModuleShape({ named: server }).kind).toBe(
    "legacy_server",
  )
  expect(detectServerModuleShape({ named: { server } }).kind).toBe(
    "legacy_server",
  )
})

test("detects tui-only and invalid plugin module shapes", () => {
  const server = () => ({})
  const tui = () => ({})

  expect(detectServerModuleShape({ default: { id: "ui", tui } }).kind).toBe(
    "tui_only",
  )
  expect(
    detectServerModuleShape({ default: { id: "mixed", server, tui } }).kind,
  ).toBe("error")
  expect(detectServerModuleShape({ named: 42 }).kind).toBe("error")
})

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "hya-compat-"))
  tempDirs.push(created)
  return created
}
