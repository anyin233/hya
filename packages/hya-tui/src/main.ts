/**
 * Entry point: `bun packages/hya-tui/src/main.ts [--server URL | --grpc HOST:PORT] [--dir PATH] [--hya PATH] [--db PATH] [--continue | --session ID | --resume [ID]] [--web-tab]`
 * (src/cli.ts `usage`). Without either explicit transport the TUI starts its own `hya serve` (src/launch.ts).
 *
 * One file, two roles (src/reload.ts): direct and WebUI starts use the Bun
 * supervisor (src/supervisor.ts), which runs this same file again as the app.
 * Bare hya supervises the app directly. Both restart it after `hya serve
 * restart`; the supervisor's environment (`HYA_TUI_RELOAD_FILE`) marks the app.
 */
import { startupMark } from "./startup"
import { existsSync } from "node:fs"
import { randomUUID } from "node:crypto"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { reloadFileEnv, takeReloadState, takeSupervision } from "./reload"
import { supervise } from "./supervisor"

async function app(): Promise<void> {
  const argv = process.argv.slice(2)
  const supervision = takeSupervision(process.env)
  const reloaded = takeReloadState(process.env)
  // Dynamic on purpose: the supervisor never loads the renderer, Solid, or the app.
  const compiled = join(import.meta.dir, "../dist/app.js")
  const built = existsSync(compiled)
  if (built) await import("./compiledRuntime")
  const { app: start } = await import(built ? compiled : "./source")
  await start({ argv, ...(supervision ? { supervision } : {}), ...(reloaded ? { reloaded } : {}) })
}

async function supervisor(): Promise<void> {
  const code = await supervise({
    bun: process.execPath,
    entry: import.meta.path,
    argv: process.argv.slice(2),
    env: process.env,
    file: join(tmpdir(), `hya-tui-reload-${process.pid}-${randomUUID()}.json`),
  })
  process.exit(code)
}

startupMark(process.env[reloadFileEnv] ? "tui_app_entry" : "tui_supervisor_entry")
const role = process.env[reloadFileEnv] ? app : supervisor
void role().catch((error: unknown) => {
  process.stderr.write(`hya-tui: ${String(error)}\n`)
  process.exitCode = 1
})
