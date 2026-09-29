/**
 * The Agents view's calls (docs/tui.md "Agents view"; the pure state is
 * state/agentsView.ts, the rendering components/AgentsView.tsx). Enter runs
 * the session on a primary agent; `m` and `t` open the shared modal picker
 * (state/picker.ts) over this view to choose the agent's default model or
 * thinking effort.
 */
import type { AgentModelSelection, AgentModelState, HyaClient } from "../client"
import type { KeyLike } from "../keys/bindings"
import {
  agentsViewKey,
  effectiveRef,
  initialAgentsView,
  settleAgentsView,
  type AgentsViewNotice,
  type AgentsViewState,
} from "../state/agentsView"
import { effortRows, modelRows } from "../state/catalog"
import { errorText } from "../state/providers"
import type { PickerSpec } from "../state/picker"
import type { AppStore } from "../state/store"

export interface AgentsViewControllerOptions {
  store: AppStore
  client: HyaClient
  /** Open the shared modal picker (components/Picker.tsx) over this view. */
  openPicker(spec: PickerSpec): void
  /** Run the session on `agent`, or remember it for the next session (commands/native.ts `selectAgent`). */
  selectAgent(agent: string): Promise<void>
}

/** The Agents view's handle: open/close it, route one key while it is open, drop in-flight calls. */
export interface AgentsViewController {
  open(): void
  close(): void
  key(pressed: KeyLike): void
  dispose(): void
}

export function createAgentsViewController({ store, client, openPicker, selectAgent }: AgentsViewControllerOptions): AgentsViewController {
  let abort: AbortController | undefined

  const view = (): AgentsViewState | undefined => store.state.agentsView
  const patch = (change: (current: AgentsViewState) => AgentsViewState): void => {
    const current = view()
    if (current) store.setAgentsView(change(current))
  }
  const notify = (notice: AgentsViewNotice | undefined): void => patch((current) => ({ ...current, notice }))
  const rowOf = (agentId: string): AgentModelState | undefined => store.state.agentModelRows.find((row) => row.agentId === agentId)
  /** Rows under the open session's binding, so its overrides show as `session`. */
  const list = (): Promise<AgentModelState[]> => client.listAgentModels(store.state.selected?.id)

  async function reload(): Promise<void> {
    try {
      const rows = await list()
      store.setAgentModelRows(rows)
      patch((current) => settleAgentsView(current, rows))
    } catch (error) {
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    }
  }

  function open(): void {
    const current = store.state.selected?.agent ?? store.state.pendingAgent
    store.setAgentsView(initialAgentsView(store.state.agentModelRows, current))
    void list()
      .then((rows) => {
        store.setAgentModelRows(rows)
        // The first open has no cached rows: land on the session's agent once they arrive.
        patch((opened) => opened.agent ? settleAgentsView(opened, rows) : initialAgentsView(rows, current))
      })
      .catch((error: unknown) => notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` }))
  }

  function close(): void {
    abort?.abort()
    abort = undefined
    store.setAgentsView(undefined)
  }

  async function refresh(): Promise<void> {
    patch((current) => ({ ...current, busy: { label: "Refreshing", startedAt: Date.now() }, notice: undefined }))
    try {
      const rows = await list()
      store.setAgentModelRows(rows)
      patch((current) => ({ ...settleAgentsView(current, rows), busy: undefined, notice: { tone: "ok", text: "Refreshed" } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    }
  }

  /** Run one cancellable write under a busy label, then re-read the rows. */
  async function write(label: string, call: (signal: AbortSignal) => Promise<string>): Promise<void> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, busy: { label, startedAt: Date.now() }, notice: undefined }))
    try {
      const text = await call(controller.signal)
      patch((current) => ({ ...current, busy: undefined, notice: { tone: "ok", text } }))
      await reload()
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      if (!controller.signal.aborted) notify({ tone: "error", text: errorText(error) })
      else await reload()
    } finally {
      if (abort === controller) abort = undefined
    }
  }

  async function select(agentId: string): Promise<void> {
    try {
      await selectAgent(agentId)
      close()
    } catch (error) {
      notify({ tone: "error", text: errorText(error) })
    }
  }

  /**
   * A pinned agent (`configured`: its model is set in a config file or by its
   * bundle) saves into the owning config file; any other agent remembers the
   * choice in the backend database.
   */
  function saveModel(row: AgentModelState, model: AgentModelSelection | undefined): Promise<void> {
    const agentId = row.agentId
    if (row.configured) {
      return write(`Saving ${agentId}'s model`, async (signal) => {
        const saved = await client.saveAgentModelConfiguration(agentId, model, signal)
        const where = saved.configurationPath ? ` · saved to ${saved.configurationPath}` : ""
        return model ? `${agentId} → ${model.providerId}/${model.modelId}${where}` : `Cleared ${agentId}'s configured model${where}`
      })
    }
    return write(model ? `Saving ${agentId}'s default` : `Clearing ${agentId}'s preference`, async (signal) => {
      await client.setAgentModel(agentId, model, undefined, signal)
      return model ? `${agentId} → ${model.providerId}/${model.modelId}` : `Cleared ${agentId}'s preference`
    })
  }

  function pickModel(agentId: string): void {
    const row = rowOf(agentId)
    if (!row) return
    openPicker({
      title: `Model · ${agentId}'s default`,
      rows: modelRows(store.state.models, effectiveRef(row)),
      hint: "Enter picks · Esc cancels",
      onSelect: (selected) => {
        const [providerId, modelId] = selected.id.split(/\/(.+)/, 2)
        if (!providerId || !modelId) return
        void saveModel(row, { providerId, modelId })
      },
    })
  }

  /** `default` clears the agent's runtime choice; the rows are the effective model's accepted labels. */
  function pickEffort(agentId: string): void {
    const row = rowOf(agentId)
    if (!row) return
    const model = store.state.models.find((candidate) => candidate.id === effectiveRef(row))
    const explicit = row.effortSource === "AGENT_EFFORT_SOURCE_PREFERENCE" ? row.effort : undefined
    openPicker({
      title: `Thinking effort · ${agentId}'s default`,
      rows: effortRows(model, explicit, row.effort || "default"),
      hint: "Enter picks · Esc cancels",
      onSelect: (selected) => {
        const effort = selected.id === "default" ? "" : selected.id
        void write(`Saving ${agentId}'s effort`, async (signal) => {
          await client.setAgentEffort(agentId, effort, signal)
          return effort ? `${agentId} effort → ${effort}` : `Cleared ${agentId}'s effort`
        })
      },
    })
  }

  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    const outcome = agentsViewKey(current, pressed, store.state.agentModelRows)
    switch (outcome.type) {
      case "update": store.setAgentsView(outcome.view); return
      case "close": close(); return
      case "cancelBusy": abort?.abort(); return
      case "refresh": void refresh(); return
      case "select": void select(outcome.agent); return
      case "pickModel": pickModel(outcome.agent); return
      case "clear": {
        const row = rowOf(outcome.agent)
        if (row) void saveModel(row, undefined)
        return
      }
      case "pickEffort": pickEffort(outcome.agent); return
    }
  }

  return { open, close, key, dispose: () => { abort?.abort() } }
}
