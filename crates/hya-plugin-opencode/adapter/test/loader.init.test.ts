import { afterEach, expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { z } from "zod"

import { loadLocalPluginHooks } from "../src/loader/init"

const HookSchema = z.object({
  marker: z.string(),
  options: z.unknown().optional(),
})

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("loads local plugin hooks sequentially and passes tuple options", async () => {
  const root = await makeTempDir()
  const first = path.join(root, "first.ts")
  const second = path.join(root, "second.ts")
  await writeFile(
    first,
    'export default { id: "first", server: async (_input, options) => ({ marker: "first", options }) }',
  )
  await writeFile(
    second,
    'export const plugin = async (_input, options) => ({ marker: "second", options })',
  )

  const loaded = await loadLocalPluginHooks(
    [pathToFileURL(first).href, [pathToFileURL(second).href, { flag: true }]],
    {},
  )

  expect(loaded.errors).toEqual([])
  expect(loaded.hooks.map((hook) => HookSchema.parse(hook))).toEqual([
    { marker: "first" },
    { marker: "second", options: { flag: true } },
  ])
})

test("loads npm plugin server entrypoints from config-relative node_modules", async () => {
  // Given: an installed npm OpenCode plugin package beside the declaring config.
  const root = await makeTempDir()
  const configFile = path.join(root, ".opencode", "opencode.json")
  const packageDir = path.join(
    root,
    "node_modules",
    "hya-test-opencode-plugin",
  )
  await mkdir(path.dirname(configFile), { recursive: true })
  await mkdir(packageDir, { recursive: true })
  await writeFile(
    path.join(packageDir, "package.json"),
    JSON.stringify({
      name: "hya-test-opencode-plugin",
      type: "module",
      main: "./wrong.js",
      exports: {
        "./server": "./server.js",
      },
    }),
  )
  await writeFile(
    path.join(packageDir, "wrong.js"),
    'export default { id: "wrong", server: async () => ({ marker: "wrong" }) }',
  )
  await writeFile(
    path.join(packageDir, "server.js"),
    'export default { id: "npm", server: async (_input, options) => ({ marker: "npm", options }) }',
  )

  // When: the loader receives the package name from an OpenCode config entry.
  const loaded = await loadLocalPluginHooks(
    [["hya-test-opencode-plugin", { source: "config" }]],
    {},
    configFile,
  )

  // Then: it imports the server entrypoint and initializes the hooks.
  expect(loaded.errors).toEqual([])
  expect(loaded.hooks.map((hook) => HookSchema.parse(hook))).toEqual([
    { marker: "npm", options: { source: "config" } },
  ])
})

test("loads npm plugin main when no server export exists", async () => {
  // Given: an installed npm plugin package that exposes server code through main.
  const root = await makeTempDir()
  const configFile = path.join(root, ".opencode", "opencode.json")
  const packageDir = path.join(
    root,
    "node_modules",
    "hya-test-main-plugin",
  )
  await mkdir(path.dirname(configFile), { recursive: true })
  await mkdir(packageDir, { recursive: true })
  await writeFile(
    path.join(packageDir, "package.json"),
    JSON.stringify({
      name: "hya-test-main-plugin",
      type: "module",
      main: "./server.js",
      exports: {
        ".": "./wrong.js",
      },
    }),
  )
  await writeFile(
    path.join(packageDir, "wrong.js"),
    'export default { id: "wrong", server: async () => ({ marker: "wrong" }) }',
  )
  await writeFile(
    path.join(packageDir, "server.js"),
    'export default { id: "main", server: async () => ({ marker: "main" }) }',
  )

  // When: the loader receives that package name from an OpenCode config entry.
  const loaded = await loadLocalPluginHooks(
    ["hya-test-main-plugin"],
    {},
    configFile,
  )

  // Then: it imports the package main as OpenCode server plugin code.
  expect(loaded.errors).toEqual([])
  expect(loaded.hooks.map((hook) => HookSchema.parse(hook))).toEqual([
    { marker: "main" },
  ])
})

test("ignores deprecated OpenCode plugin package names", async () => {
  const loaded = await loadLocalPluginHooks(
    ["opencode-openai-codex-auth", "opencode-copilot-auth"],
    {},
  )

  expect(loaded.errors).toEqual([])
  expect(loaded.hooks).toEqual([])
})

test("isolates import and init failures while preserving later plugins", async () => {
  const root = await makeTempDir()
  const badImport = path.join(root, "bad-import.ts")
  const badInit = path.join(root, "bad-init.ts")
  const good = path.join(root, "good.ts")
  await writeFile(badImport, 'throw new Error("boom import")')
  await writeFile(
    badInit,
    'export default { id: "bad", server: async () => { throw new Error("boom init") } }',
  )
  await writeFile(
    good,
    'export default { id: "good", server: async () => ({ marker: "good" }) }',
  )

  const loaded = await loadLocalPluginHooks(
    [
      pathToFileURL(badImport).href,
      pathToFileURL(badInit).href,
      pathToFileURL(good).href,
    ],
    {},
  )

  expect(loaded.hooks.map((hook) => HookSchema.parse(hook))).toEqual([
    { marker: "good" },
  ])
  expect(loaded.errors).toHaveLength(2)
  expect(loaded.errors.map((error) => error.spec)).toEqual([
    pathToFileURL(badImport).href,
    pathToFileURL(badInit).href,
  ])
})

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "hya-opencode-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}
