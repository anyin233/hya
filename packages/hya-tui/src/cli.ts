/** Command-line options for the TUI entry point (docs/tui.md "Start it"). */
import { resolve } from "node:path"

export interface Options {
  /** Base URL of a running `hya serve`; unset = the TUI starts its own backend (src/launch.ts). */
  server?: string
  /** Workspace directory: `x-hya-directory` of every request, and the started backend's working directory. */
  directory: string
  /** `hya` binary for the started backend (`--hya`); else `HYA_BIN`, else `hya` on PATH. */
  hya?: string
  /** SQLite database of the started backend (`--db`); default `$XDG_STATE_HOME/hya/sessions.db`. */
  db?: string
  /** Open the most recent top-level session of `directory` (`--continue`). */
  continue: boolean
  /** Open this session (`--session <id>`). */
  session?: string
}

export const usage = `Usage: bun packages/hya-tui/src/main.ts [options]

Without --server the TUI starts its own backend (hya serve on a free local
port, working directory --dir) and stops it when the TUI exits.

Options:
  --server URL      Connect to a running hya serve instead of starting one
  --dir PATH        Workspace directory (default: the current directory)
  --hya PATH        hya binary to start; lookup order: --hya, then HYA_BIN,
                    then hya on PATH
  --db PATH         SQLite database of the started backend
                    (default: $XDG_STATE_HOME/hya/sessions.db, else
                    ~/.local/state/hya/sessions.db)
  -c, --continue    Open the most recent top-level session in --dir
  -s, --session ID  Open the session with this id
  -h, --help        Show this help
`

/** Parse the flags above; returns null for `--help`. */
export function parseArguments(argv: string[], cwd = process.cwd()): Options | null {
  let server: string | undefined
  let directory = cwd
  let hya: string | undefined
  let db: string | undefined
  let session: string | undefined
  let resume = false
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index]
    const value = argv[index + 1]
    if (arg === "--help" || arg === "-h") return null
    if (arg === "--continue" || arg === "-c") resume = true
    else if (value === undefined) throw new Error(`Unknown or incomplete option: ${arg}`)
    else if (arg === "--server") server = argv[++index]!
    else if (arg === "--dir") directory = argv[++index]!
    else if (arg === "--hya") hya = argv[++index]!
    else if (arg === "--db") db = argv[++index]!
    else if (arg === "--session" || arg === "-s") session = argv[++index]!
    else throw new Error(`Unknown or incomplete option: ${arg}`)
  }
  if (resume && session) throw new Error("--continue and --session cannot be combined")
  if (server !== undefined && (hya !== undefined || db !== undefined)) throw new Error("--hya and --db only apply without --server")
  const options: Options = { directory: resolve(directory), continue: resume }
  if (server !== undefined) {
    const url = new URL(server)
    if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("--server needs an HTTP URL")
    options.server = url.toString()
  }
  if (hya !== undefined) options.hya = hya
  if (db !== undefined) options.db = db
  if (session !== undefined) options.session = session
  return options
}
