import type { CliRenderer } from "@opentui/core"

let tearingDown = false

/**
 * Report whether renderer teardown has started. Past this point the SDK context
 * has aborted its live event stream, so a late `AbortError` is expected.
 */
export function isTearingDown() {
  return tearingDown
}

export function destroyRenderer(renderer: Pick<CliRenderer, "isDestroyed" | "setTerminalTitle" | "destroy">) {
  tearingDown = true
  renderer.setTerminalTitle("")
  if (renderer.isDestroyed) return
  renderer.destroy()
}
