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
 * `/connect-remote` runs `hya bridge` (the same binary lookup as the daemon
 * start: `--hya`, `HYA_BIN`, PATH; src/bridge.ts) and moves to its loopback
 * URL; `/disconnect-remote` comes back to the database's daemon (found or
 * started) or to a fixed local `--server`. A TUI started remote (bare
 * `hya --connect`: `--remote --server-label`, no `--db`) has no local
 * backend to come back to. Its `--server` is bare `hya --connect`'s bridge,
 * whose token comes in `HYA_SERVER_TOKEN` (never argv): read once at
 * startup and removed from the environment together with `HYA_RELAY_LINK`,
 * so no child (editor, shell, bridge) inherits either.
 *
 * Lifecycle: every way out runs `shutdown()` once: restore the terminal,
 * settle the open session (app/sessionKeeper.ts: archive a used one on a
 * graceful exit, at most 2 s), then exit.
 * The backend daemon keeps running. The way out decides the session's fate:
 * Ctrl+C twice or `/exit` archive it (graceful); Ctrl+D or `/to-background`
 * leave it running; a signal (SIGINT, SIGTERM, SIGHUP; the WebUI host sends
 * SIGHUP when its tab closes) never archives. An unused session this client
 * created costs nothing on the way out: the daemon drops it once no client
 * watches it (a kill included). A backend that
 * cannot be reached or started is reported on stderr with the tail of its
 * output, and the TUI exits with status 1 before it takes over the terminal.
 * After `hya serve restart` a supervised TUI (src/main.ts) reloads: it asks
 * its supervisor to start it again on the open session with the unsent
 * draft (src/reload.ts) and exits with status 75, leaving the session as a
 * signal does. A supervisor that disappears ends the TUI (SIGHUP's status).
 */
import { createCliRenderer, type CliRenderer } from "@opentui/core"
import { render } from "@opentui/solid"
import type { Options } from "../cli"
import { HyaClient } from "../client"
import { GrpcHyaClient } from "../grpc_client"
import { BackendError, connectOrStart, defaultDatabase, findRunningServer, probeHealth, resolveHyaBinary, type Connection } from "../launch"
import { startBridge, takeServerToken } from "../bridge"
import { loadPreferences, preferencesPath } from "../prefs"
import { setTheme } from "../theme"
import { createAppStore, type BackendInfo } from "../state/store"
import { App } from "./App"
import { AppContext } from "./context"
import { createController, type Controller } from "./controller"
import type { ExitMode } from "./sessionKeeper"
import { reloadArguments, reloadExitCode, requestReload, type ReloadState, type Supervision } from "../reload"

const exitSignals = { SIGINT: 130, SIGTERM: 143, SIGHUP: 129 } as const

const sameUrl = (a: string, b: string): boolean => a.replace(/\/+$/, "") === b.replace(/\/+$/, "")

/** How this app process was started (src/main.ts). */
export interface Launch {
  /** The command line after the entry script: the base of a reload's (src/reload.ts `reloadArguments`). */
  argv: string[]
  /** Run by the supervisor (src/supervisor.ts): only then can the app reload itself. */
  supervision?: Supervision
  /** Started by a reload: the draft to put back. */
  reloaded?: ReloadState
}

/** The status notice of an app a reload started. */
export const reloadedNotice = "TUI reloaded (hya serve restart)"

export async function run(options: Options, launch: Launch = { argv: [] }): Promise<void> {
  // First, before anything can spawn a child. The token belongs to `--server` only.
  const envToken = takeServerToken()
  let serverToken = options.server ? envToken : undefined
  let renderer: CliRenderer | undefined
  let controller: Controller | undefined
  let grpcClient: GrpcHyaClient | undefined
  const store = createAppStore()
  let stopping = false
  const shutdown = async (code: number, mode: ExitMode): Promise<void> => {
    // Once: `renderer.destroy()` re-enters here synchronously (its "destroy" event).
    if (stopping) return
    stopping = true
    try {
      if (renderer && !renderer.isDestroyed) renderer.destroy()
    } catch {
      // The terminal is being torn down anyway.
    }
    await controller?.close(mode).catch(() => undefined)
    grpcClient?.close()
    process.exit(code)
  }
  // A signal is never a graceful exit: the session keeps running (no archive).
  for (const [signal, code] of Object.entries(exitSignals)) process.on(signal, () => void shutdown(code, "signal"))
  // The supervisor went away (killed outright): its terminal belongs to nobody now; leave it.
  const supervision = launch.supervision
  if (supervision) setInterval(() => { if (process.ppid !== supervision.parent) void shutdown(exitSignals.SIGHUP, "signal") }, 1000).unref()

  // The database whose daemon this TUI uses: `--db`, or the default without an explicit transport.
  const db = options.grpc ? undefined : options.db ?? (options.server ? undefined : defaultDatabase(process.env))
  const connect = (): Promise<Connection> => {
    const binary = resolveHyaBinary({ flag: options.hya, env: process.env })
    return connectOrStart({ bin: binary.path, directory: options.directory, db: db! })
  }
  const daemonInfo = (connection: Connection): BackendInfo => ({ pid: connection.pid, db: connection.db, startedAt: connection.startedAt })

  let server = options.grpc ? `grpc://${options.grpc}` : options.server
  let backend: BackendInfo | undefined
  try {
    if (server && db && !(await probeHealth(server, fetch, undefined, serverToken))) {
      process.stdout.write(`hya-tui: ${server} does not answer; using the hya server daemon of ${db}…\n`)
      server = undefined
      serverToken = undefined
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

  // Remote: no directory scope until a Project is chosen (--dir is this machine's).
  grpcClient = options.grpc ? new GrpcHyaClient(options.grpc, options.remote ? "" : options.directory) : undefined
  const client = grpcClient ?? new HyaClient(server, options.remote ? "" : options.directory, fetch, serverToken)
  store.setServerUrl(server)
  if (options.serverLabel) store.setServerLabel(options.serverLabel)
  store.setBackend(backend)
  if (options.web) store.setWeb(options.web)
  if (options.webTab) store.setWebTab(true)
  if (loaded.preferences.vim) store.setVim(true)
  if (loaded.preferences.paneLayout) store.setPaneLayout(loaded.preferences.paneLayout)
  // `hya serve restart` replaced the backend: start this TUI again from its files (src/reload.ts), on the open session with the unsent draft.
  const reload = supervision && db
    ? () => {
        const draft = controller?.ui.composerInput
        try {
          requestReload(supervision.file, {
            argv: reloadArguments(launch.argv, { ...(store.state.selected ? { session: store.state.selected.id } : {}), server: client.baseUrl }),
            ...(draft?.text ? { draft } : {}),
          })
        } catch (error) {
          store.setStatus(`TUI reload failed: ${error instanceof Error ? error.message : String(error)} · restart hya for the new TUI`)
          return
        }
        void shutdown(reloadExitCode, "signal")
      }
    : undefined
  controller = createController({
    client, store, directory: options.directory, remote: options.remote === true,
    quit: (mode) => void shutdown(0, mode),
    startup: { continue: options.continue, ...(options.session ? { session: options.session } : {}), ...(options.resume ? { resume: options.resume } : {}) },
    connectionHint: db
      ? `the hya server daemon of ${db} did not answer · hya serve status`
      : options.grpc
        ? `the gRPC listener ${options.grpc} did not answer · check hya serve and HYA_GRPC_BIND`
      : options.serverLabel
        ? "the remote backend did not answer · check the link and that its hya serve --relay runs"
        : "start hya serve or drop --server",
    bridge: (link, flags, onLine) => startBridge({ bin: resolveHyaBinary({ flag: options.hya, env: process.env }).path, link, flags, onLine }),
    ...(db
      ? {
          home: async () => {
            const connection = await connect()
            store.setBackend(daemonInfo(connection))
            return { url: connection.url, pid: connection.pid, started: connection.started, generation: `${connection.pid}:${connection.startedAt}`, version: connection.version, startedAt: connection.startedAt }
          },
        }
      : options.grpc && !options.remote
        ? {
            home: async () => {
              await client.request("GET", "/v1/health")
              store.setBackend({ explicit: true })
              return { url: `grpc://${options.grpc}`, pid: 0, started: false }
            },
          }
      : options.server && !options.remote
        ? {
            home: async () => {
              const fixed = options.server!
              if (!(await probeHealth(fixed))) throw new Error(`${fixed} does not answer`)
              store.setBackend({ explicit: true })
              return { url: fixed, pid: 0, started: false }
            },
          }
        : {}),
    ...(db
      ? {
          reconnect: async () => {
            const connection = await connect()
            store.setBackend(daemonInfo(connection))
            return { url: connection.url, pid: connection.pid, started: connection.started, generation: `${connection.pid}:${connection.startedAt}`, version: connection.version, startedAt: connection.startedAt }
          },
          // Never starts one: after `hya serve stop` / `restart` (app/reconnect.ts).
          find: async () => {
            const found = await findRunningServer(db, options.directory)
            if (!found) return undefined
            store.setBackend({ pid: found.pid, db, startedAt: found.startedAt })
            return { url: found.url, pid: found.pid, started: false, generation: `${found.pid}:${found.startedAt}`, version: found.version, startedAt: found.startedAt }
          },
        }
      : {}),
    ...(reload ? { onRestarted: reload } : {}),
    preferencesPath: prefsPath,
    preferredPermissionMode: loaded.preferences.permissionMode,
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
  // Every full-screen view and the main layout follow the terminal size
  // (`useTerminalDimensions`, one "resize" listener each, all mounted at
  // once). Past Node's default of 10 its MaxListenersExceededWarning would be
  // printed on stderr, over the TUI.
  renderer.setMaxListeners(64)
  // OpenTUI's own signal handlers destroy the renderer; finish the shutdown from there.
  // Only reached when nothing above started the shutdown (a signal OpenTUI caught first): never archive.
  renderer.once("destroy", () => void shutdown(0, "signal"))
  const active = controller
  if (launch.reloaded?.draft) active.ui.composerInput = launch.reloaded.draft
  await render(() => (
    <AppContext.Provider value={{ store, controller: active, server, ui: active.ui }}>
      <App />
    </AppContext.Provider>
  ), renderer)
  await controller.start()
  if (launch.reloaded) warnings.unshift(reloadedNotice)
  if (warnings.length) store.setStatus([store.state.status, ...warnings].filter(Boolean).join(" · "))
}
