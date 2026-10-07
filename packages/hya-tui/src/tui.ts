/**
 * The app role of src/main.ts (the process its supervisor runs): parse the
 * command line and start the TUI (app/run.tsx).
 *
 * Source launches register the JSX plugin in source.ts; packaged launches
 * precompile this entry and use compiledRuntime.ts for Solid client mapping.
 * The dynamic app import keeps argument/help handling cheap.
 */
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
