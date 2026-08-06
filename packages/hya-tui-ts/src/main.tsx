import { realpath } from "node:fs/promises"
import { parseArgs } from "node:util"
import { Effect } from "effect"

import { HyaPaths, HyaPlatform } from "./hya/platform"
import { createStaticPluginHost } from "./hya/static-host"
import { startupMark } from "./hya/startup-trace"
import { run, type TuiInput } from "./upstream"
import { resolve } from "./upstream/config"

/**
 * Parse Bun CLI argv and start the TypeScript TUI against a running backend.
 *
 * **Requires `--url`.** This package is frontend-only and will not start
 * `hya-backend`. Prefer `hya` / `hya-ts` for normal use; they supply `--url`.
 *
 * @param argv - CLI arguments after the script name (typically `process.argv.slice(2)`)
 * @param runner - Optional TUI runner; defaults to Effect-backed `run` with `HyaPlatform`
 * @returns Promise that resolves when the runner completes
 * @throws Error when `--url` is missing or the URL/project path is invalid
 */
export async function launch(argv: string[], runner: (input: TuiInput) => Promise<unknown> = runTui) {
  startupMark("bun_entry")
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    strict: true,
    options: {
      url: { type: "string" },
      project: { type: "string" },
      continue: { type: "boolean" },
      session: { type: "string" },
      fork: { type: "boolean" },
      prompt: { type: "string" },
      agent: { type: "string" },
      model: { type: "string" },
    },
  })
  if (!values.url) throw new Error("--url is required")
  const url = new URL(values.url).toString()
  const directory = await realpath(values.project ?? positionals[0] ?? process.cwd())
  process.chdir(directory)

  return runner({
    url,
    directory,
    args: {
      continue: values.continue,
      sessionID: values.session,
      fork: values.fork,
      prompt: values.prompt,
      agent: values.agent,
      model: values.model,
    },
    config: resolve({}, { terminalSuspend: process.platform !== "win32" }),
    pluginHost: createStaticPluginHost(),
  })
}

/**
 * Default runner: provide `HyaPaths` via Effect and enter the upstream TUI `run`.
 *
 * @param input - Parsed launch input (url, directory, args, config, plugin host)
 * @returns Promise that resolves when the TUI Effect program finishes
 */
function runTui(input: TuiInput) {
  return Effect.runPromise(run(input).pipe(Effect.provideService(HyaPlatform, HyaPaths)))
}

if (import.meta.main) {
  await launch(process.argv.slice(2))
}
