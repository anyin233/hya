import { expect, test } from "bun:test"
import { mkdtemp, readFile, realpath, rm, stat } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

import { createHeapSnapshotWriter } from "../src/hya/heap-snapshot"

test("heap snapshot writer returns a unique path containing a parseable V8 snapshot", async () => {
  const root = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-heap-snapshot-")))
  try {
    const first = await createHeapSnapshotWriter(root)()
    const second = await createHeapSnapshotWriter(root)()
    expect(first).toHaveLength(1)
    expect(second).toHaveLength(1)
    const firstPath = first[0]
    const secondPath = second[0]
    if (!firstPath || !secondPath) throw new Error("heap snapshot writer returned no path")
    expect(path.dirname(firstPath)).toBe(root)
    expect(path.dirname(secondPath)).toBe(root)
    expect(firstPath).not.toBe(secondPath)
    if (process.platform !== "win32") expect((await stat(firstPath)).mode & 0o777).toBe(0o600)

    const parsed = JSON.parse(await readFile(firstPath, "utf8")) as {
      snapshot?: { meta?: unknown }
      nodes?: unknown
      edges?: unknown
      strings?: unknown
    }
    expect(parsed.snapshot?.meta).toBeDefined()
    expect(Array.isArray(parsed.nodes)).toBe(true)
    expect(Array.isArray(parsed.edges)).toBe(true)
    expect(Array.isArray(parsed.strings)).toBe(true)
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

test("heap snapshot writer propagates unsupported generation errors", async () => {
  const root = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-heap-snapshot-error-")))
  try {
    const writer = createHeapSnapshotWriter(root, () => {
      throw new Error("heap snapshots unsupported")
    })
    await expect(writer()).rejects.toThrow("heap snapshots unsupported")
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})
