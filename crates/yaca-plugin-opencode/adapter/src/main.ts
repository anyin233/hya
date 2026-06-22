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

console.error(
  "yaca-opencode-adapter skeleton: OpenCode plugin loading is not implemented yet",
)
process.exit(1)
