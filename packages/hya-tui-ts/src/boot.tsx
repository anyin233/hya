/**
 * Early boot entry: start loading the TUI graph immediately, wait for the
 * backend URL on a FIFO in parallel, then launch.
 *
 * `hya-ts` creates the FIFO, spawns this entry, and writes the listen URL once
 * `hya-backend` is ready — overlapping Bun module load with backend bind.
 *
 * Reads one line with `fs.readSync` (does not wait for EOF). Waiting for EOF
 * deadlocks when the parent keeps the FIFO write end open for the child lifetime.
 */
import { closeSync, openSync, readSync } from "node:fs"
import { startupMark } from "./hya/startup-trace"

startupMark("bun_entry", "boot")

function readUrlLine(fifo: string): string {
  startupMark("boot_wait_url", fifo)
  const fd = openSync(fifo, "r")
  try {
    const buf = Buffer.alloc(4096)
    const n = readSync(fd, buf, 0, buf.length, null)
    const text = buf.subarray(0, n).toString("utf8").trim()
    const line = text.split(/\r?\n/).find((row) => row.length > 0)
    if (!line) throw new Error(`empty URL from FIFO ${fifo}`)
    startupMark("boot_got_url")
    return line
  } finally {
    closeSync(fd)
  }
}

async function readUrl(): Promise<string> {
  const fromEnv = process.env.HYA_SERVER_URL?.trim()
  if (fromEnv) return fromEnv

  const fifo = process.env.HYA_SERVER_URL_FIFO?.trim()
  if (!fifo) {
    throw new Error("boot requires HYA_SERVER_URL or HYA_SERVER_URL_FIFO")
  }
  return readUrlLine(fifo)
}

// Kick off the heavy graph while we block on the backend URL.
const appPromise = import("./main.tsx")
const url = await readUrl()
const { launch } = await appPromise

const argv = process.argv.slice(2).filter((arg, index, all) => {
  if (arg === "--url") return false
  if (index > 0 && all[index - 1] === "--url") return false
  return true
})
argv.unshift("--url", url)
await launch(argv)
