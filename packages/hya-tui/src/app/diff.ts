/**
 * The Diff view's calls (docs/tui.md "Diff view"; the pure state is
 * state/diff.ts, the rendering components/DiffView.tsx). Scroll outcomes go
 * to `ui.diff` (app/context.ts), the same handle-registration pattern the
 * transcript uses.
 */
import type { HyaClient } from "../client"
import type { UiHandles } from "./context"
import type { KeyLike } from "../keys/bindings"
import { diffViewKey, initialDiffView, parseDiff, settleDiffView, type DiffViewState } from "../state/diff"
import { errorText } from "../state/providers"
import type { AppStore } from "../state/store"

export interface DiffControllerOptions {
  store: AppStore
  client: HyaClient
  ui: UiHandles
}

export function createDiffController({ store, client, ui }: DiffControllerOptions) {
  let abort: AbortController | undefined

  const view = (): DiffViewState | undefined => store.state.diffView
  const patch = (change: (current: DiffViewState) => DiffViewState): void => {
    const current = view()
    if (current) store.setDiffView(change(current))
  }

  async function load(): Promise<void> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, busy: { label: "Loading the diff", startedAt: Date.now() }, notice: undefined }))
    try {
      const raw = await client.getVcsDiff()
      const files = parseDiff(raw, client.directory)
      patch((current) => ({ ...settleDiffView(current, files), busy: undefined }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      if (!controller.signal.aborted) patch((current) => ({ ...current, notice: { tone: "error", text: errorText(error) } }))
    } finally {
      if (abort === controller) abort = undefined
    }
  }

  function open(): void {
    store.setDiffView(initialDiffView([]))
    void load()
  }

  function close(): void {
    abort?.abort()
    abort = undefined
    store.setDiffView(undefined)
  }

  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    const outcome = diffViewKey(current, pressed)
    switch (outcome.type) {
      case "update": store.setDiffView(outcome.view); return
      case "close": close(); return
      case "cancelBusy": abort?.abort(); return
      case "reload": void load(); return
      case "scroll": {
        const scroller = ui.diff
        if (!scroller) return
        switch (outcome.action) {
          case "line-up": scroller.line(-1); return
          case "line-down": scroller.line(1); return
          case "page-up": scroller.page(-1); return
          case "page-down": scroller.page(1); return
          case "top": scroller.top(); return
          case "bottom": scroller.bottom(); return
        }
      }
    }
  }

  return { open, close, key, dispose: () => { abort?.abort() } }
}

export type DiffController = ReturnType<typeof createDiffController>
