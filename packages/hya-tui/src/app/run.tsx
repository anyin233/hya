/**
 * Start the TUI: the backend (one-command launch, src/launch.ts) unless
 * `--server` names one, then the renderer, store, controller, Solid tree,
 * and the initial load.
 *
 * Lifecycle: every way out — Ctrl+C twice, Ctrl+D, `/exit`, or a signal
 * (SIGINT, SIGTERM, SIGHUP; the WebUI host sends SIGHUP when its tab
 * closes) — runs `shutdown()` once: restore the terminal, stop the started
 * backend (SIGTERM, then SIGKILL after its grace period) and wait for it to
 * exit, then exit. A backend that fails to start is reported on stderr with
 * the tail of its output, and the TUI exits with status 1 before it takes
 * over the terminal.
 */
import { createCliRenderer, type CliRenderer } from "@opentui/core"
import { render } from "@opentui/solid"
import type { Options } from "../cli"
import { HyaClient } from "../client"
import { BackendError, defaultDatabase, resolveHyaBinary, startBackend, type Backend } from "../launch"
import { createAppStore } from "../state/store"
import { App } from "./App"
import { AppContext } from "./context"
import { createController, type Controller } from "./controller"

const exitSignals = { SIGINT: 130, SIGTERM: 143, SIGHUP: 129 } as const

export async function run(options: Options): Promise<void> {
  let backend: Backend | undefined
  let renderer: CliRenderer | undefined
  let controller: Controller | undefined
  let stopping: Promise<void> | undefined
  const shutdown = (code: number): Promise<void> => {
    stopping ??= (async () => {
      try {
        if (renderer && !renderer.isDestroyed) renderer.destroy()
      } catch {
        // The terminal is being torn down anyway.
      }
      controller?.dispose()
      await backend?.stop().catch(() => undefined)
      process.exit(code)
    })()
    return stopping
  }
  for (const [signal, code] of Object.entries(exitSignals)) process.on(signal, () => void shutdown(code))

  let server = options.server
  if (!server) {
    try {
      const binary = resolveHyaBinary({ flag: options.hya, env: process.env })
      process.stdout.write(`hya-tui: starting hya serve (${binary.path}, found by ${binary.source}) in ${options.directory}…\n`)
      backend = await startBackend({ bin: binary.path, directory: options.directory, db: options.db ?? defaultDatabase(process.env) })
      server = backend.url
    } catch (error) {
      const detail = error instanceof BackendError && error.detail ? `\n--- hya serve output (last lines) ---\n${error.detail}\n` : ""
      process.stderr.write(`hya-tui: could not start the backend: ${error instanceof Error ? error.message : String(error)}${detail}\n`)
      await backend?.stop()
      process.exit(1)
    }
    // The backend died under the TUI: say so instead of retrying forever.
    void backend.exited.then((code) => {
      if (stopping) return
      renderer?.destroy()
      process.stderr.write(`hya-tui: the backend exited unexpectedly (code ${code ?? "signal"})\n${backend?.outputTail() ?? ""}\n`)
      process.exit(1)
    })
  }

  const client = new HyaClient(server, options.directory)
  const store = createAppStore()
  if (backend) store.setBackend({ pid: backend.pid, bin: backend.bin, db: backend.db })
  controller = createController({
    client, store, directory: options.directory,
    quit: () => void shutdown(0),
    startup: { continue: options.continue, ...(options.session ? { session: options.session } : {}) },
    connectionHint: backend ? "the started backend did not answer" : "start hya serve or drop --server",
  })
  // autoFocus off: a click (on the transcript, a Thinking line, the sidebar)
  // must not move focus from the one input to a scrollbox. Ctrl+C is the
  // composer's double-press quit (components/Composer.tsx), not the renderer's.
  renderer = await createCliRenderer({ exitOnCtrlC: false, targetFps: 30, autoFocus: false })
  // OpenTUI's own signal handlers destroy the renderer; finish the shutdown from there.
  renderer.once("destroy", () => void shutdown(0))
  const active = controller
  await render(() => (
    <AppContext.Provider value={{ store, controller: active, server, ui: {} }}>
      <App />
    </AppContext.Provider>
  ), renderer)
  await controller.start()
}
