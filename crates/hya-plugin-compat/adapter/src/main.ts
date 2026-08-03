import path from "node:path"

import { runAdapter } from "./runtime"

const VERSION = "0.0.0"

function printHelp() {
  console.log(`hya-compat-adapter ${VERSION}`)
  console.log(
    "Usage: bun run src/main.ts [--help|--version|--bundle-extension <absolute-path> ...]",
  )
}

type StartupOptions =
  | { readonly kind: "run"; readonly bundleExtensions: readonly string[] }
  | { readonly kind: "help" }
  | { readonly kind: "version" }
  | { readonly kind: "error"; readonly message: string }

function parseStartupArgs(args: readonly string[]): StartupOptions {
  const normalized = args[0] === "--" ? args.slice(1) : args
  if (normalized.length === 1 && normalized[0] === "--version") {
    return { kind: "version" }
  }
  if (
    normalized.length === 1 &&
    (normalized[0] === "--help" || normalized[0] === "-h")
  ) {
    return { kind: "help" }
  }
  if (normalized.length === 0) {
    return { kind: "run", bundleExtensions: [] }
  }

  const bundleExtensions: string[] = []
  for (let index = 0; index < normalized.length; index += 2) {
    if (normalized[index] !== "--bundle-extension") {
      return {
        kind: "error",
        message: `unknown startup argument: ${normalized[index] ?? ""}`,
      }
    }
    const extension = normalized[index + 1]
    if (extension === undefined || !path.isAbsolute(extension)) {
      return {
        kind: "error",
        message: "--bundle-extension requires an absolute path",
      }
    }
    bundleExtensions.push(extension)
  }
  return { kind: "run", bundleExtensions: Object.freeze(bundleExtensions) }
}

const startup = parseStartupArgs(Bun.argv.slice(2))
switch (startup.kind) {
  case "version":
    console.log(VERSION)
    process.exit(0)
  case "help":
    printHelp()
    process.exit(0)
  case "error":
    console.error(startup.message)
    process.exit(1)
  case "run":
    await runAdapter({
      input: Bun.stdin.stream(),
      stdout: { write: (data) => process.stdout.write(data) },
      stderr: { write: (data) => process.stderr.write(data) },
      version: VERSION,
      bundleExtensions: startup.bundleExtensions,
      env: process.env,
    })
}
