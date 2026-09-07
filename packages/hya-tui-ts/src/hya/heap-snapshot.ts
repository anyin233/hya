import { randomUUID } from "node:crypto"
import { mkdir, stat, writeFile } from "node:fs/promises"
import path from "node:path"

export type HeapSnapshotGenerator = () => string
export type HeapSnapshotWriter = () => Promise<string[]>

/**
 * Create a Bun-compatible V8 heap snapshot writer rooted in an owned cache
 * directory. The returned callback resolves only after the file is present and
 * non-empty so the UI cannot report a path for a failed write.
 */
export function createHeapSnapshotWriter(
  directory: string,
  generate: HeapSnapshotGenerator = () => Bun.generateHeapSnapshot("v8"),
): HeapSnapshotWriter {
  return async () => {
    await mkdir(directory, { recursive: true })
    const filename = `heap-${Date.now()}-${randomUUID()}.heapsnapshot`
    const file = path.join(directory, filename)
    await writeFile(file, generate(), { mode: 0o600, flag: "wx" })
    const details = await stat(file)
    if (!details.isFile() || details.size === 0) {
      throw new Error(`Heap snapshot was not written to ${file}`)
    }
    return [file]
  }
}
