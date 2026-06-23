import { runAdapter } from "./runtime"

const VERSION = "0.0.0"

function printHelp() {
  console.log(`yaca-opencode-adapter ${VERSION}`)
  console.log("Usage: bun run src/main.ts [--help|--version]")
}

const arg = Bun.argv[2]

if (arg === "--version") {
  console.log(VERSION)
  process.exit(0)
}

if (arg === "--help" || arg === "-h") {
  printHelp()
  process.exit(0)
}

await runAdapter({
  input: Bun.stdin.stream(),
  stdout: { write: (data) => process.stdout.write(data) },
  stderr: { write: (data) => process.stderr.write(data) },
  version: VERSION,
  env: process.env,
})
