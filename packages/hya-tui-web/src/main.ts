import { resolve } from "node:path"
import { startHost } from "./host"

const usage = "Usage: bun packages/hya-tui-web/src/main.ts [--host 127.0.0.1] [--port 7681] [--cwd DIR] -- <command> [args...]\n"

function parse(argv: string[]) {
  const split = argv.indexOf("--")
  const flags = split === -1 ? argv : argv.slice(0, split)
  const command = split === -1 ? [] : argv.slice(split + 1)
  let hostname = "127.0.0.1"
  let port = 7681
  let cwd = process.cwd()
  for (let index = 0; index < flags.length; index++) {
    const flag = flags[index]
    const value = flags[index + 1]
    if (flag === "--help" || flag === "-h") return null
    if (flag === "--host" && value) hostname = value
    else if (flag === "--port" && value && /^\d+$/.test(value)) port = Number(value)
    else if (flag === "--cwd" && value) cwd = resolve(value)
    else throw new Error(`Unknown or incomplete option: ${flag}`)
    index++
  }
  if (command.length === 0) throw new Error("Missing command after --")
  return { hostname, port, cwd, command }
}

try {
  const options = parse(process.argv.slice(2))
  if (!options) {
    process.stdout.write(usage)
  } else {
    const host = startHost(options)
    process.stdout.write(`hya-tui-web listening on ${host.url}\n`)
    // SIGINT/SIGTERM/SIGHUP: stop serving and end every tab's process
    // (SIGHUP, then SIGKILL after a grace period) before exiting.
    let stopping = false
    const stop = () => {
      if (stopping) return
      stopping = true
      void host.stop().finally(() => process.exit(0))
    }
    process.on("SIGINT", stop)
    process.on("SIGTERM", stop)
    process.on("SIGHUP", stop)
  }
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n${usage}`)
  process.exit(2)
}
