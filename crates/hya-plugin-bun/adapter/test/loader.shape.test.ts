import { afterEach, expect, test } from "bun:test"
import { mkdir, rm, writeFile } from "node:fs/promises"
import path from "node:path"

import {
  detectServerModuleShape,
  resolveExtensionTarget,
} from "../src/loader/shape"
import { cleanupTempDirs, makeTempDir } from "./helpers"

afterEach(async () => {
  await cleanupTempDirs()
})

test("resolves absolute extension files to file URLs", async () => {
  const root = await makeTempDir()
  const file = path.join(root, "extension.ts")
  await writeFile(file, "export default {}")

  const resolved = await resolveExtensionTarget(file)
  expect(resolved).toMatch(/^file:\/\//)
  expect(resolved.endsWith("extension.ts")).toBe(true)
})

test("resolves extension directories to index files", async () => {
  const root = await makeTempDir()
  const indexFile = path.join(root, "my-extension", "index.ts")
  await mkdir(path.dirname(indexFile), { recursive: true })
  await writeFile(indexFile, "export default {}")

  const resolved = await resolveExtensionTarget(path.join(root, "my-extension"))
  expect(resolved.endsWith("my-extension/index.ts")).toBe(true)
})

test("rejects extension directories without package.json or index file", async () => {
  const root = await makeTempDir()
  const empty = path.join(root, "empty")
  await mkdir(empty, { recursive: true })

  await expect(resolveExtensionTarget(empty)).rejects.toThrow(
    /missing package\.json or index file/,
  )
})

test("detects supported extension module shapes", () => {
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

test("detects tui-only and invalid extension module shapes", () => {
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
