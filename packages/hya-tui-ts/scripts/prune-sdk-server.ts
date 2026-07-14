import { readFile, rename, rm, writeFile } from "node:fs/promises"
import path from "node:path"

const runtime = Bun.argv[2]
if (!runtime) throw new Error("runtime directory is required")

const sdk = path.join(runtime, "node_modules/@opencode-ai/sdk")
const packagePath = path.join(sdk, "package.json")
const manifest = JSON.parse(await readFile(packagePath, "utf8")) as { exports?: Record<string, unknown> }
const exportMap = manifest.exports
const client = exportMap?.["./v2/client"]
if (!exportMap || !client) throw new Error("pinned SDK v2 client export was not found")

delete exportMap["."]
delete exportMap["./server"]
exportMap["./v2"] = client
delete exportMap["./v2/server"]
const nextPackagePath = `${packagePath}.hya-${process.pid}`
await writeFile(nextPackagePath, `${JSON.stringify(manifest, null, 2)}\n`)
await rename(nextPackagePath, packagePath)
await Promise.all(
  [
    "index.js",
    "index.d.ts",
    "server.js",
    "server.d.ts",
    "process.js",
    "process.d.ts",
    "v2/index.js",
    "v2/index.d.ts",
    "v2/server.js",
    "v2/server.d.ts",
  ].map((file) => rm(path.join(sdk, "dist", file), { force: true })),
)

const probe = Bun.spawnSync(
  [process.execPath, "-e", 'import { createOpencodeClient } from "@opencode-ai/sdk/v2"; if (typeof createOpencodeClient !== "function") process.exit(1)'],
  { cwd: runtime },
)
if (probe.exitCode !== 0) throw new Error(`pruned SDK client import failed: ${probe.stderr.toString()}`)
