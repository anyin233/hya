// Timestamp immediately before spawning the real executable. The PTY and
// rendering still belong to tui-web; this wrapper never reads terminal bytes.
import { appendFileSync } from "node:fs"
const trace = process.env.HYA_STARTUP_TRACE_FILE
if (!trace) throw new Error("HYA_STARTUP_TRACE_FILE is required")
appendFileSync(trace, JSON.stringify({ hya_startup: true, mark: "frontend_spawn", wall_ms: performance.timeOrigin + performance.now(), pid: process.pid }) + "\n")
const child = Bun.spawn(process.argv.slice(2), { stdin: "inherit", stdout: "inherit", stderr: "inherit" })
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) process.on(signal, () => child.kill(signal))
process.exit(await child.exited)
