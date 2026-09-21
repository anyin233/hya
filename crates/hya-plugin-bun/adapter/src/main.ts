import path from "node:path"

import { runAdapter } from "./runtime"

const VERSION = "1.0.0"

function printHelp() {
  console.log(`hya-bun-adapter ${VERSION}`)
  console.log(
    "Usage: bun run src/main.ts [--help|--version|--bundle-extension <absolute-path> ...|--extension <absolute-path> ...]",
  )
}

type StartupOptions =
  | { readonly kind: "run"; readonly extensions: readonly string[] }
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
    return { kind: "run", extensions: [] }
  }

  const extensions: string[] = []
  for (let index = 0; index < normalized.length; index += 2) {
    const flag = normalized[index]
    if (flag !== "--bundle-extension" && flag !== "--extension") {
      return {
        kind: "error",
        message: `unknown startup argument: ${flag ?? ""}`,
      }
    }
    const extension = normalized[index + 1]
    if (extension === undefined || !isExtensionSpecifier(extension)) {
      return {
        kind: "error",
        message: `${flag} requires an absolute path`,
      }
    }
    extensions.push(extension)
  }
  return { kind: "run", extensions: Object.freeze(extensions) }
}

function isExtensionSpecifier(value: string): boolean {
  return path.isAbsolute(value) || value.startsWith("file://")
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
      extensions: startup.extensions,
      env: process.env,
    })
}
