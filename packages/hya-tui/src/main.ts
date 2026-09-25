/**
 * Entry point: `bun packages/hya-tui/src/main.ts [--server URL] [--dir PATH] [--hya PATH] [--db PATH] [--continue | --session ID]`
 * (src/cli.ts `usage`). Without `--server` the TUI starts its own `hya serve` (src/launch.ts).
 *
 * Registers the Solid JSX transform before any `.tsx` module or solid-js is
 * loaded. bunfig.toml preloads only apply to the directory Bun runs in, so the
 * entry registers the plugin itself and loads the app with a dynamic import.
 */
import "@opentui/solid/preload"
import { parseArguments, usage } from "./cli"

async function main(): Promise<void> {
  const options = parseArguments(process.argv.slice(2))
  if (!options) {
    process.stdout.write(usage)
    return
  }
  const { run } = await import("./app/run")
  await run(options)
}

void main().catch((error: unknown) => {
  process.stderr.write(`hya-tui: ${String(error)}\n`)
  process.exitCode = 1
})
