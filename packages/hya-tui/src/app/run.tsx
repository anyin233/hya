/**
 * Start the TUI: the backend (src/launch.ts; ADR-0023: the database's
 * running daemon, else one started with `hya serve start`) unless
 * `--server` names one, the preferences file (src/prefs.ts: the saved theme
 * and vim mode), then the renderer, store, controller, Solid tree, and the
 * initial load.
 *
 * `--server` with `--db` (bare `hya` passes both to every TUI it starts,
 * WebUI tabs included): the URL is tried first; when it does not answer, the
 * database's daemon is found or started instead.
 *
 * When the TUI knows its database it also replaces a lost server
 * (app/reconnect.ts). A fixed `--server` without `--db` is never replaced.
 *
 * Lifecycle: every way out — Ctrl+C twice, Ctrl+D, `/exit`, or a signal
 * (SIGINT, SIGTERM, SIGHUP; the WebUI host sends SIGHUP when its tab
 * closes) — runs `shutdown()` once: restore the terminal, delete this
 * client's session if it created it and never used it (app/sessionKeeper.ts,
 * at most 2 s), then exit. The backend daemon keeps running. A backend that
 * cannot be reached or started is reported on stderr with the tail of its
 * output, and the TUI exits with status 1 before it takes over the terminal.
 */
import { createCliRenderer, type CliRenderer } from "@opentui/core"
import { render } from "@opentui/solid"
import type { Options } from "../cli"
import { HyaClient } from "../client"
import { BackendError, connectOrStart, defaultDatabase, findRunningServer, probeHealth, resolveHyaBinary, type Connection } from "../launch"
import { loadPreferences, preferencesPath } from "../prefs"
import { setTheme } from "../theme"
import { createAppStore, type BackendInfo } from "../state/store"
import { App } from "./App"
import { AppContext } from "./context"
import { createController, type Controller } from "./controller"

const exitSignals = { SIGINT: 130, SIGTERM: 143, SIGHUP: 129 } as const

const sameUrl = (a: string, b: string): boolean => a.replace(/\/+$/, "") === b.replace(/\/+$/, "")

export async function run(options: Options): Promise<void> {
  let renderer: CliRenderer | undefined
  let controller: Controller | undefined
  let stopping = false
  const shutdown = async (code: number): Promise<void> => {
    // Once: `renderer.destroy()` re-enters here synchronously (its "destroy" event).
    if (stopping) return
    stopping = true
    try {
      if (renderer && !renderer.isDestroyed) renderer.destroy()
    } catch {
      // The terminal is being torn down anyway.
    }
    await controller?.close().catch(() => undefined)
    process.exit(code)
  }
  for (const [signal, code] of Object.entries(exitSignals)) process.on(signal, () => void shutdown(code))

  // The database whose daemon this TUI uses: `--db`, or the default without `--server`.
  const db = options.db ?? (options.server ? undefined : defaultDatabase(process.env))
  const connect = (): Promise<Connection> => {
    const binary = resolveHyaBinary({ flag: options.hya, env: process.env })
    return connectOrStart({ bin: binary.path, directory: options.directory, db: db! })
  }
  const daemonInfo = (connection: Connection): BackendInfo => ({ pid: connection.pid, db: connection.db, startedAt: connection.startedAt })

  let server = options.server
  let backend: BackendInfo | undefined
  try {
    if (server && db && !(await probeHealth(server))) {
      process.stdout.write(`hya-tui: ${server} does not answer; using the hya server daemon of ${db}…\n`)
      server = undefined
    }
    if (!server) {
      process.stdout.write(`hya-tui: connecting to the hya server daemon of ${db}, or starting one…\n`)
      const connection = await connect()
      server = connection.url
      backend = daemonInfo(connection)
      process.stdout.write(`hya-tui: ${connection.started ? "started" : "using"} the hya server daemon pid ${connection.pid} at ${connection.url}\n`)
    } else if (db) {
      // Bare `hya`'s TUIs: name the daemon behind the URL for `/status`.
      const found = await findRunningServer(db, options.directory)
      backend = found && sameUrl(found.url, server) ? { pid: found.pid, db, startedAt: found.startedAt } : { db }
    } else {
      backend = { explicit: true }
    }
  } catch (error) {
    const detail = error instanceof BackendError && error.detail ? `\n--- hya serve start output (last lines) ---\n${error.detail}\n` : ""
    process.stderr.write(`hya-tui: could not reach or start the hya server: ${error instanceof Error ? error.message : String(error)}${detail}\n`)
    process.exit(1)
  }

  // Preferences first, so the first frame already uses the saved theme.
  const prefsPath = preferencesPath(process.env)
  const loaded = loadPreferences(prefsPath)
  const warnings = loaded.warning ? [loaded.warning] : []
  if (loaded.preferences.theme && !setTheme(loaded.preferences.theme)) {
    warnings.push(`Unknown theme ${loaded.preferences.theme} in ${prefsPath}; using hya`)
  }

  const client = new HyaClient(server, options.directory)
  const store = createAppStore()
  store.setServerUrl(server)
  store.setBackend(backend)
  if (options.web) store.setWeb(options.web)
  if (loaded.preferences.vim) store.setVim(true)
  controller = createController({
    client, store, directory: options.directory,
    quit: () => void shutdown(0),
    startup: { continue: options.continue, ...(options.session ? { session: options.session } : {}) },
    connectionHint: db ? `the hya server daemon of ${db} did not answer · hya serve status` : "start hya serve or drop --server",
    ...(db
      ? {
          reconnect: async () => {
            const connection = await connect()
            store.setBackend(daemonInfo(connection))
            return { url: connection.url, pid: connection.pid, started: connection.started, version: connection.version, startedAt: connection.startedAt }
          },
          // Never starts one: after `hya serve stop` / `restart` (app/reconnect.ts).
          find: async () => {
            const found = await findRunningServer(db, options.directory)
            if (!found) return undefined
            store.setBackend({ pid: found.pid, db, startedAt: found.startedAt })
            return { url: found.url, pid: found.pid, started: false, version: found.version, startedAt: found.startedAt }
          },
        }
      : {}),
    preferencesPath: prefsPath,
    // The renderer exists once the first frame is due; these run on user actions after that.
    terminal: {
      copy: (text) => renderer?.copyToClipboardOSC52(text) ?? false,
      suspend: () => renderer?.suspend(),
      resume: () => renderer?.resume(),
      // OSC 9 / OSC 777 (src/notify.ts): no visible effect, so this can go
      // straight to stdout rather than through the renderer's own frame
      // buffer (which has no generic "write this sequence" method).
      notify: (sequence) => { process.stdout.write(sequence) },
      // CliRenderer already tracks the terminal's CSI ?1004h focus reporting
      // (FocusIn/FocusOut) and emits "focus"/"blur"; no hand-rolling needed.
      onFocusChange: (handler) => {
        if (!renderer) return () => {}
        const onFocus = () => handler(true)
        const onBlur = () => handler(false)
        renderer.on("focus", onFocus)
        renderer.on("blur", onBlur)
        return () => {
          renderer?.off("focus", onFocus)
          renderer?.off("blur", onBlur)
        }
      },
    },
  })
  // autoFocus off: a click (on the transcript, a Thinking line, the sidebar)
  // must not move focus from the one input to a scrollbox. Ctrl+C is the
  // composer's double-press quit (components/Composer.tsx), not the renderer's.
  renderer = await createCliRenderer({ exitOnCtrlC: false, targetFps: 30, autoFocus: false })
  // OpenTUI's own signal handlers destroy the renderer; finish the shutdown from there.
  renderer.once("destroy", () => void shutdown(0))
  const active = controller
  await render(() => (
    <AppContext.Provider value={{ store, controller: active, server, ui: active.ui }}>
      <App />
    </AppContext.Provider>
  ), renderer)
  await controller.start()
  if (warnings.length) store.setStatus([store.state.status, ...warnings].filter(Boolean).join(" · "))
}
