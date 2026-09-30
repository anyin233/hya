/**
 * Entry point: `bun packages/hya-tui/src/main.ts [--server URL | --grpc HOST:PORT] [--dir PATH] [--hya PATH] [--db PATH] [--continue | --session ID | --resume [ID]] [--web-tab]`
 * (src/cli.ts `usage`). Without either explicit transport the TUI starts its own `hya serve` (src/launch.ts).
 *
 * One file, two roles (src/reload.ts): started by a host it is the
 * supervisor (src/supervisor.ts), which runs this same file again as the app
 * (src/tui.ts) and starts it anew when the app reloads after `hya serve
 * restart`. The supervisor's environment (`HYA_TUI_RELOAD_FILE`) marks the app.
 */
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
  const { app: start } = await import("./tui")
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

const role = process.env[reloadFileEnv] ? app : supervisor
void role().catch((error: unknown) => {
  process.stderr.write(`hya-tui: ${String(error)}\n`)
  process.exitCode = 1
})
