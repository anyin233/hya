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
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}
