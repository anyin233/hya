/**
 * Production bundle entry for hya-tui-ts.
 *
 * Uses the OpenTUI Solid Bun plugin so dependencies (except native
 * platform packages) are inlined — cold start then avoids re-transpiling
 * the 180+ module TypeScript graph on every launch.
 */
import solidPlugin from "@opentui/solid/bun-plugin"
import { mkdirSync, rmSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const root = join(dirname(fileURLToPath(import.meta.url)), "..")
const outdir = join(root, "dist")

/** Native OpenTUI binaries resolved at runtime from node_modules. */
const platformExternals = [
  "@opentui/core-darwin-arm64",
  "@opentui/core-darwin-x64",
  "@opentui/core-linux-arm64",
  "@opentui/core-linux-arm64-musl",
  "@opentui/core-linux-x64",
  "@opentui/core-linux-x64-musl",
  "@opentui/core-win32-arm64",
  "@opentui/core-win32-x64",
]

rmSync(outdir, { recursive: true, force: true })
mkdirSync(outdir, { recursive: true })

const result = await Bun.build({
  entrypoints: [join(root, "src/main.tsx"), join(root, "src/boot.tsx")],
  target: "bun",
  outdir,
  minify: true,
  splitting: true,
  plugins: [solidPlugin],
  external: platformExternals,
})

if (!result.success) {
  for (const log of result.logs) console.error(log)
  process.exit(1)
}

const mains = result.outputs.filter((o) => o.path.endsWith(".js")).map((o) => o.path)
console.log(`hya-tui-ts: bundled ${mains.length} js outputs into ${outdir}`)
for (const path of mains) {
  console.log(`  ${path}`)
}
