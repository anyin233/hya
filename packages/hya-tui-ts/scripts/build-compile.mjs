import solidPlugin from "@opentui/solid/bun-plugin"
import { rmSync, renameSync, existsSync } from "fs"

const platformExternals = [
  "@opentui/core-darwin-arm64",
  "@opentui/core-darwin-x64",
  "@opentui/core-linux-arm64",
  "@opentui/core-linux-arm64-musl",
  "@opentui/core-linux-x64-musl",
  "@opentui/core-win32-arm64",
  "@opentui/core-win32-x64",
]

rmSync("/tmp/hya-tui-compiled", { force: true })

const result = await Bun.build({
  entrypoints: ["./src/main.tsx"],
  target: "bun",
  minify: true,
  plugins: [solidPlugin],
  external: platformExternals,
  compile: {
    outfile: "/tmp/hya-tui-compiled",
    autoloadBunfig: false,
    autoloadDotenv: false,
  },
})
if (!result.success) {
  for (const log of result.logs) console.error(log)
  process.exit(1)
}
console.log("compile ok")
