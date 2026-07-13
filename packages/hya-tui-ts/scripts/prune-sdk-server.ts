import { readFile, rm, writeFile } from "node:fs/promises"
import path from "node:path"

const runtime = Bun.argv[2]
if (!runtime) throw new Error("runtime directory is required")

const sdk = path.join(runtime, "node_modules/@opencode-ai/sdk")
const packagePath = path.join(sdk, "package.json")
const manifest = JSON.parse(await readFile(packagePath, "utf8")) as { exports?: Record<string, unknown> }
const exportMap = manifest.exports
if (!exportMap || !("./server" in exportMap) || !("./v2/server" in exportMap)) {
  throw new Error("pinned SDK server exports were not found")
}

delete exportMap["./server"]
delete exportMap["./v2/server"]
await writeFile(packagePath, `${JSON.stringify(manifest, null, 2)}\n`)
await Promise.all(
  ["server.js", "server.d.ts", "v2/server.js", "v2/server.d.ts", "process.js", "process.d.ts"].map((file) =>
    rm(path.join(sdk, "dist", file), { force: true }),
  ),
)
