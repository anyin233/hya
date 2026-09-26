/**
 * The Saved Rules view's calls (docs/tui.md "Saved Rules"; the pure state is
 * state/rules.ts, the rendering components/RulesView.tsx).
 */
import type { HyaClient } from "../client"
import type { KeyLike } from "../keys/bindings"
import type { AppStore } from "../state/store"
import { errorText } from "../state/providers"
import { initialRulesView, rulesViewKey, settleRulesView, type RulesNotice, type RulesViewState } from "../state/rules"

export interface RulesControllerOptions {
  store: AppStore
  client: HyaClient
}

export function createRulesController({ store, client }: RulesControllerOptions) {
  let abort: AbortController | undefined

  const view = (): RulesViewState | undefined => store.state.rulesView
  const patch = (change: (current: RulesViewState) => RulesViewState): void => {
    const current = view()
    if (current) store.setRulesView(change(current))
  }
  const notify = (notice: RulesNotice | undefined): void => patch((current) => ({ ...current, notice }))

  async function reload(): Promise<void> {
    try {
      const rules = await client.listSavedRules()
      store.setSavedRules(rules)
      patch((current) => settleRulesView(current, rules))
    } catch (error) {
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    }
  }

  function open(): void {
    store.setRulesView(initialRulesView(store.state.savedRules))
    void reload()
  }

  function close(): void {
    abort?.abort()
    abort = undefined
    store.setRulesView(undefined)
  }

  async function deleteRule(id: string): Promise<void> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, confirm: undefined, busy: { kind: "delete", label: `Deleting ${id}`, startedAt: Date.now() } }))
    try {
      await client.deleteSavedRule(id, controller.signal)
      patch((current) => ({ ...current, busy: undefined, notice: { tone: "ok", text: `Deleted rule ${id}` } }))
      await reload()
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      if (!controller.signal.aborted) notify({ tone: "error", text: `Delete failed: ${errorText(error)}` })
      else await reload()
    } finally {
      if (abort === controller) abort = undefined
    }
  }

  async function refresh(): Promise<void> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, busy: { kind: "refresh", label: "Refreshing", startedAt: Date.now() } }))
    try {
      const rules = await client.listSavedRules()
      store.setSavedRules(rules)
      patch((current) => ({ ...settleRulesView(current, rules), busy: undefined, notice: { tone: "ok", text: "Refreshed" } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    } finally {
      if (abort === controller) abort = undefined
    }
  }

  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    const outcome = rulesViewKey(current, pressed, store.state.savedRules)
    switch (outcome.type) {
      case "update": store.setRulesView(outcome.view); return
      case "close": close(); return
      case "cancelBusy": abort?.abort(); return
      case "refresh": void refresh(); return
      case "delete": void deleteRule(outcome.rule); return
    }
  }

  return { open, close, key, dispose: () => { abort?.abort() } }
}

export type RulesController = ReturnType<typeof createRulesController>
