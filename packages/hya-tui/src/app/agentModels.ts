/**
 * The Agent Models view's calls (docs/tui.md "Agent Models"; the pure state
 * is state/agentModels.ts, the rendering components/AgentModelsView.tsx).
 * Enter opens the shared modal picker (state/picker.ts) over this view to
 * choose an agent's remembered default model.
 */
import type { AgentModelState, HyaClient } from "../client"
import type { KeyLike } from "../keys/bindings"
import {
  agentModelsViewKey,
  initialAgentModelsView,
  settleAgentModelsView,
  type AgentModelsNotice,
  type AgentModelsViewState,
} from "../state/agentModels"
import { modelRows } from "../state/catalog"
import { errorText } from "../state/providers"
import type { PickerSpec } from "../state/picker"
import type { AppStore } from "../state/store"

export interface AgentModelsControllerOptions {
  store: AppStore
  client: HyaClient
  /** Open the shared modal picker (components/Picker.tsx) over this view. */
  openPicker(spec: PickerSpec): void
}

export function createAgentModelsController({ store, client, openPicker }: AgentModelsControllerOptions) {
  let abort: AbortController | undefined

  const view = (): AgentModelsViewState | undefined => store.state.agentModelsView
  const patch = (change: (current: AgentModelsViewState) => AgentModelsViewState): void => {
    const current = view()
    if (current) store.setAgentModelsView(change(current))
  }
  const notify = (notice: AgentModelsNotice | undefined): void => patch((current) => ({ ...current, notice }))

  async function reload(): Promise<void> {
    try {
      const rows = await client.listAgentModels()
      store.setAgentModelRows(rows)
      patch((current) => settleAgentModelsView(current, rows))
    } catch (error) {
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    }
  }

  function open(): void {
    store.setAgentModelsView(initialAgentModelsView(store.state.agentModelRows))
    void reload()
  }

  function close(): void {
    abort?.abort()
    abort = undefined
    store.setAgentModelsView(undefined)
  }

  async function refresh(): Promise<void> {
    patch((current) => ({ ...current, busy: { label: "Refreshing", startedAt: Date.now() }, notice: undefined }))
    try {
      const rows = await client.listAgentModels()
      store.setAgentModelRows(rows)
      patch((current) => ({ ...settleAgentModelsView(current, rows), busy: undefined, notice: { tone: "ok", text: "Refreshed" } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    }
  }

  async function setPreference(agentId: string, preference: { providerId: string; modelId: string } | undefined, label: string): Promise<void> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, busy: { label, startedAt: Date.now() }, notice: undefined }))
    try {
      await client.setAgentModel(agentId, preference, undefined, controller.signal)
      patch((current) => ({ ...current, busy: undefined, notice: { tone: "ok", text: preference ? `${agentId} → ${preference.providerId}/${preference.modelId}` : `Cleared ${agentId}'s preference` } }))
      await reload()
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      if (!controller.signal.aborted) notify({ tone: "error", text: errorText(error) })
      else await reload()
    } finally {
      if (abort === controller) abort = undefined
    }
  }

  function pickModel(agentId: string): void {
    const row = store.state.agentModelRows.find((candidate) => candidate.agentId === agentId)
    const current = row?.effective?.providerId && row.effective.modelId ? `${row.effective.providerId}/${row.effective.modelId}` : ""
    openPicker({
      title: `Model · ${agentId}'s default`,
      rows: modelRows(store.state.models, current),
      hint: "Enter picks · Esc cancels",
      onSelect: (selected) => {
        const [providerId, modelId] = selected.id.split(/\/(.+)/, 2)
        if (!providerId || !modelId) return
        void setPreference(agentId, { providerId, modelId }, `Saving ${agentId}'s default`)
      },
    })
  }

  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    const outcome = agentModelsViewKey(current, pressed, store.state.agentModelRows)
    switch (outcome.type) {
      case "update": store.setAgentModelsView(outcome.view); return
      case "close": close(); return
      case "cancelBusy": abort?.abort(); return
      case "refresh": void refresh(); return
      case "pickModel": pickModel(outcome.agent); return
      case "clear": void setPreference(outcome.agent, undefined, `Clearing ${outcome.agent}'s preference`); return
    }
  }

  return { open, close, key, dispose: () => { abort?.abort() } }
}

export type AgentModelsController = ReturnType<typeof createAgentModelsController>
