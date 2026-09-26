/** Command-line options for the TUI entry point (docs/tui.md "Start it"). */
import { resolve } from "node:path"

export interface Options {
  /** Base URL of a running `hya serve`; unset = the TUI finds or starts its database's daemon (src/launch.ts). */
  server?: string
  /** Workspace directory: `x-hya-directory` of every request, and the started backend's working directory. */
  directory: string
  /** `hya` binary that starts the daemon (`--hya`); else `HYA_BIN`, else `hya` on PATH. */
  hya?: string
  /**
   * The database whose daemon the TUI uses (`--db`); default
   * `$XDG_STATE_HOME/hya/sessions.db` without `--server`. With `--server` it
   * names the database behind that URL, so the TUI can find or restart its
   * daemon when the server goes away; without it, `--server` is fixed.
   */
  db?: string
  /** Open the most recent top-level session of `directory` (`--continue`). */
  continue: boolean
  /** Open this session (`--session <id>`). */
  session?: string
  /** The WebUI bare `hya` serves next to this TUI (`--web-url`), or why it could not (`--web-error`). */
  web?: WebInfo
}

/** The WebUI state bare `hya` passes to its terminal TUI: exactly one of the two is set. */
export interface WebInfo {
  /** `--web-url <url>`: the WebUI's address (shown in the status bar, sidebar, and `/status`). */
  url?: string
  /** `--web-error <reason>`: the WebUI could not start (shown as a warning notice). */
  error?: string
}

export const usage = `Usage: bun packages/hya-tui/src/main.ts [options]

Without --server the TUI uses the backend daemon of --db: the server already
running on it (its <db>.server.json answers), else a new one it starts with
\`hya serve start\` (detached, working directory --dir). The daemon keeps
running after the TUI exits; \`hya serve stop\` stops it, and the TUI then
starts nothing until /reconnect. After \`hya serve restart\` it attaches to
the new daemon; after a crash it finds or starts the next one.

Options:
  --server URL      Connect to this hya server instead of the database's
                    daemon; with --db, a lost server is replaced by the
                    database's daemon
  --dir PATH        Workspace directory (default: the current directory)
  --hya PATH        hya binary that starts the daemon; lookup order: --hya,
                    then HYA_BIN, then hya on PATH
  --db PATH         SQLite database whose daemon to use
                    (default without --server: $XDG_STATE_HOME/hya/sessions.db,
                    else ~/.local/state/hya/sessions.db)
  -c, --continue    Open the most recent top-level session in --dir
  -s, --session ID  Open the session with this id
  --web-url URL     Show this WebUI address (set by bare hya, which serves
                    the WebUI next to this TUI)
  --web-error TEXT  Show "WebUI unavailable: TEXT" (set by bare hya when the
                    WebUI could not start)
  -h, --help        Show this help

Environment:
  HYA_TUI_CONFIG    TUI preferences file (theme); default
                    $XDG_CONFIG_HOME/hya/tui.json, else ~/.config/hya/tui.json
`

/** Parse the flags above; returns null for `--help`. */
export function parseArguments(argv: string[], cwd = process.cwd()): Options | null {
  let server: string | undefined
  let directory = cwd
  let hya: string | undefined
  let db: string | undefined
  let session: string | undefined
  let webUrl: string | undefined
  let webError: string | undefined
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
    else if (arg === "--web-url") webUrl = argv[++index]!
    else if (arg === "--web-error") webError = argv[++index]!
    else throw new Error(`Unknown or incomplete option: ${arg}`)
  }
  if (resume && session) throw new Error("--continue and --session cannot be combined")
  const options: Options = { directory: resolve(directory), continue: resume }
  if (server !== undefined) {
    const url = new URL(server)
    if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("--server needs an HTTP URL")
    options.server = url.toString()
  }
  if (hya !== undefined) options.hya = hya
  if (db !== undefined) options.db = db
  if (session !== undefined) options.session = session
  if (webUrl !== undefined && webError !== undefined) throw new Error("--web-url and --web-error cannot be combined")
  if (webUrl !== undefined) {
    const url = new URL(webUrl)
    if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("--web-url needs an HTTP URL")
    options.web = { url: url.toString() }
  }
  if (webError !== undefined) options.web = { error: webError }
  return options
}
