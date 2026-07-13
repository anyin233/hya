import { realpath } from "node:fs/promises"
import { parseArgs } from "node:util"
import { Effect } from "effect"

import { HyaPaths, HyaPlatform } from "./hya/platform"
import { createStaticPluginHost } from "./hya/static-host"
import { run, type TuiInput } from "./upstream"
import { resolve } from "./upstream/config"

export async function launch(argv: string[], runner: (input: TuiInput) => Promise<unknown> = runTui) {
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

function runTui(input: TuiInput) {
  return Effect.runPromise(run(input).pipe(Effect.provideService(HyaPlatform, HyaPaths)))
}

if (import.meta.main) {
  await launch(process.argv.slice(2))
}
