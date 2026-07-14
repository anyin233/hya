import { expect, test } from "bun:test"
import { copyFile, cp, mkdtemp, readFile, rm } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

const root = path.resolve(import.meta.dir, "..")

test("prepared runtime retains only an importable SDK v2 client", async () => {
  const runtime = await mkdtemp(path.join(os.tmpdir(), "hya-sdk-runtime-"))
  const sdk = path.join(runtime, "node_modules/@opencode-ai/sdk")
  try {
    await Promise.all(
      ["package.json", "bun.lock", "bunfig.toml", "tsconfig.json"].map((file) =>
        copyFile(path.join(root, file), path.join(runtime, file)),
      ),
    )
    await cp(path.join(root, "src"), path.join(runtime, "src"), { recursive: true })
    const install = Bun.spawnSync([process.execPath, "install", "--frozen-lockfile", "--production", "--force"], {
      cwd: runtime,
      env: { ...process.env, BUN_INSTALL_CACHE_DIR: path.join(runtime, ".bun-cache") },
    })
    expect(install.exitCode, install.stderr.toString()).toBe(0)

    const prune = Bun.spawnSync([process.execPath, path.join(root, "scripts/prune-sdk-server.ts"), runtime])
    expect(prune.exitCode, prune.stderr.toString()).toBe(0)

    const probe = Bun.spawnSync(
      [
        process.execPath,
        "-e",
        'import { createOpencodeClient } from "@opencode-ai/sdk/v2"; if (typeof createOpencodeClient !== "function") process.exit(1)',
      ],
      { cwd: runtime },
    )
    expect(probe.exitCode, probe.stderr.toString()).toBe(0)

    const build = Bun.spawnSync(
      [process.execPath, "build", "src/main.tsx", "--outdir", "dist", "--target", "bun", "--packages", "external"],
      { cwd: runtime },
    )
    expect(build.exitCode, build.stderr.toString()).toBe(0)

    const manifest = JSON.parse(await readFile(path.join(sdk, "package.json"), "utf8"))
    expect(manifest.exports["./v2"]).toEqual(manifest.exports["./v2/client"])
    expect(manifest.exports["."]).toBeUndefined()
    expect(manifest.exports["./server"]).toBeUndefined()
    expect(manifest.exports["./v2/server"]).toBeUndefined()
    for (const file of [
      "dist/index.js",
      "dist/index.d.ts",
      "dist/server.js",
      "dist/server.d.ts",
      "dist/process.js",
      "dist/process.d.ts",
      "dist/v2/index.js",
      "dist/v2/index.d.ts",
      "dist/v2/server.js",
      "dist/v2/server.d.ts",
    ]) {
      expect(await Bun.file(path.join(sdk, file)).exists(), file).toBe(false)
    }
  } finally {
    await rm(runtime, { recursive: true, force: true })
  }
}, 30_000)
