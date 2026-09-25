/** Start the TUI: renderer, store, controller, Solid tree, then the initial load. */
import { createCliRenderer } from "@opentui/core"
import { render } from "@opentui/solid"
import type { Options } from "../cli"
import { HyaClient } from "../client"
import { createAppStore } from "../state/store"
import { App } from "./App"
import { AppContext } from "./context"
import { createController } from "./controller"

export async function run(options: Options): Promise<void> {
  const client = new HyaClient(options.server, options.directory)
  const store = createAppStore()
  let quit = (): void => undefined
  const controller = createController({ client, store, directory: options.directory, quit: () => quit() })
  // autoFocus off: a click (on the transcript, a Thinking line, the sidebar)
  // must not move focus from the one input to a scrollbox. Ctrl+C is the
  // composer's double-press quit (components/Composer.tsx), not the renderer's.
  const renderer = await createCliRenderer({ exitOnCtrlC: false, targetFps: 30, autoFocus: false })
  quit = () => {
    // destroy() restores the terminal; the exit is forced shortly after in case
    // an in-flight request (a running shell turn) keeps the event loop alive.
    renderer.destroy()
    setTimeout(() => process.exit(0), 250).unref()
  }
  renderer.once("destroy", () => controller.dispose())
  await render(() => (
    <AppContext.Provider value={{ store, controller, server: options.server, ui: {} }}>
      <App />
    </AppContext.Provider>
  ), renderer)
  await controller.start()
}
