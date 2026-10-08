/** Precompile JSX once when packaging, keeping native dependencies external. */
import solidPlugin from "@opentui/solid/bun-plugin"
import { mkdir, rename, rm } from "node:fs/promises"
import { join } from "node:path"

const outdir = join(import.meta.dir, "dist")
await mkdir(outdir, { recursive: true })
const result = await Bun.build({
  entrypoints: [join(import.meta.dir, "src/tui.ts")],
  target: "bun",
  packages: "external",
  minify: true,
  splitting: true,
  naming: { entry: "app.js", chunk: "[name]-[hash].js" },
  plugins: [solidPlugin],
})
if (!result.success) throw new AggregateError(result.logs, "TUI build failed")
const pending = join(outdir, `app.js.${process.pid}.tmp`)
try {
  const entry = result.outputs.find((output) => output.kind === "entry-point")!
  for (const output of result.outputs) {
    if (output !== entry) await Bun.write(join(outdir, output.path), output)
  }
  await Bun.write(pending, entry)
  await rename(pending, join(outdir, "app.js"))
} finally {
  await rm(pending, { force: true })
}
