/** Opt-in phase timestamps, written away from the terminal under test. */
import { appendFileSync } from "node:fs"

export function startupMark(mark: string, detail?: string): void {
  const path = process.env.HYA_STARTUP_TRACE_FILE
  if (!path) return
  const wall_ms = performance.timeOrigin + performance.now()
  try {
    appendFileSync(path, JSON.stringify({ hya_startup: true, mark, wall_ms, pid: process.pid, ...(detail ? { detail } : {}) }) + "\n")
  } catch { /* Diagnostics must never prevent startup. */ }
}
