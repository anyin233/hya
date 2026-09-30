/**
 * The app role of src/main.ts (the process its supervisor runs): parse the
 * command line and start the TUI (app/run.tsx).
 *
 * Registers the Solid JSX transform before any `.tsx` module or solid-js is
 * loaded. bunfig.toml preloads only apply to the directory Bun runs in, so
 * this module registers the plugin itself and loads the app with a dynamic
 * import.
 */
import "@opentui/solid/preload"
import { parseArguments, usage } from "./cli"
import type { Launch } from "./app/run"

export async function app(launch: Launch): Promise<void> {
  const options = parseArguments(launch.argv)
  if (!options) {
    process.stdout.write(usage)
    return
  }
  // Dynamic on purpose: the JSX plugin above must be registered before any `.tsx` module loads.
  const { run } = await import("./app/run")
  await run(options, launch)
}
