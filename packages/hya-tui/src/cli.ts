/** Command-line options for the TUI entry point. */
import { resolve } from "node:path"

export interface Options {
  server: string
  directory: string
}

export const usage = "Usage: bun packages/hya-tui/src/main.ts [--server http://127.0.0.1:8080] [--dir PATH]\n"

/** Parse `--server` / `--dir`; returns null for `--help`. */
export function parseArguments(argv: string[], cwd = process.cwd()): Options | null {
  let server = "http://127.0.0.1:8080"
  let directory = cwd
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index]
    if (arg === "--help" || arg === "-h") return null
    if (arg === "--server" && argv[index + 1]) server = argv[++index]!
    else if (arg === "--dir" && argv[index + 1]) directory = argv[++index]!
    else throw new Error(`Unknown or incomplete option: ${arg}`)
  }
  const url = new URL(server)
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("--server needs an HTTP URL")
  return { server: url.toString(), directory: resolve(directory) }
}
