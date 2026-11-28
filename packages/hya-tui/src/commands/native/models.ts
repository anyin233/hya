import { revertReadOnlyStatus } from "../../app/revert"
import { HttpError, type SessionInfo } from "../../client"
import { effortRows, isKnownEffort, modelRows } from "../../state/catalog"
import { currentModel, modelBaseReference, modelReference } from "../../state/format"
import { defaultModelRef, invalidEffortSuffix } from "../../state/providers"
import { matchValues, type ArgumentPosition, type CommandContext, type CommandInvocation, type CommandSpec } from "../registry"

/**
 * Open the `/model` picker: models grouped by provider, the session's model
 * (or the pending choice) marked. With no session the choice is remembered
 * for the next one. The Provider View opens it too (after adding a provider
 * while the session runs on `hya/offline`): `title` and `highlight` (a
 * provider id: its first model is highlighted) are set then.
 */
export function openModelPicker({ store, client, actions }: CommandContext, options: { title?: string; highlight?: string; onChosen?: (model: string) => void } = {}): void {
  if (store.state.selected?.parent) {
    store.setStatus(revertReadOnlyStatus)
    return
  }
  const selected = store.state.selected
  const current = selected ? modelReference(selected) : (store.state.pendingModel ?? "")
  // Rows are loaded from the catalog already in `state.models` (no async loading state in the picker itself).
  const rows = modelRows(store.state.models, current)
  const first = options.highlight ? rows.findIndex((row) => row.tag === options.highlight) : -1
  actions.openPicker({
    title: options.title ?? "Model",
    // `current` decides the opening highlight: move it to the provider's first model.
    rows: first < 0 ? rows : rows.map((row, index) => ({ ...row, current: index === first })),
    onSelect: async (row) => {
      if (store.state.selected?.parent) {
        store.setStatus(revertReadOnlyStatus)
        return
      }
      const session = store.state.selected
      const reference = row.id
      if (!session) {
        store.setPendingModel(reference)
        store.setStatus(`Model → ${reference} · applies when the session is created`)
        options.onChosen?.(reference)
        return
      }
      const updated = await client.updateSessionModel(session.id, reference)
      store.setSelected(updated)
      await persistAgentModel({ store, client, actions }, updated, reference)
      store.setStatus(`Model → ${reference}`)
      options.onChosen?.(reference)
      await actions.refresh()
    },
  })
}

/**
 * Run the session on `agent` (`UpdateSession`); with no session open, remember
 * it for the next `CreateSession`. Shared by `/agent <name>` and the Agents view.
 */
export async function selectAgent({ store, client, actions }: CommandContext, agent: string): Promise<void> {
  if (store.state.selected?.parent) {
    store.setStatus(revertReadOnlyStatus)
    return
  }
  const selected = store.state.selected
  if (!selected) {
    store.setPendingAgent(agent)
    store.setStatus(`Agent → ${agent} · applies when the session is created`)
    return
  }
  store.setSelected(await client.updateSession(selected.id, { agent }))
  store.setStatus(`Agent → ${agent}`)
  await actions.refresh()
}

/**
 * Persist `/model`'s choice for the active agent, including an explicit `#effort` suffix.
 * An agent pinned by `agents.<id>.model` answers 409: the config owns its default, so the
 * choice stays a session override (the session model already carries the suffix).
 */
async function persistAgentModel(context: CommandContext, session: SessionInfo, reference: string): Promise<void> {
  const [base, effort] = reference.split("#", 2)
  const [providerId, ...modelParts] = base!.split("/")
  const modelId = modelParts.join("/")
  if (!providerId || !modelId) return
  try {
    await context.client.setAgentModel(session.agent, { providerId, modelId }, session.id)
  } catch (error) {
    if (error instanceof HttpError && error.status === 409) return
    throw error
  }
  if (effort) await context.client.setAgentEffort(session.agent, effort)
}

function effortModelBase(context: CommandContext): string {
  const { store } = context
  if (store.state.selected) return modelBaseReference(store.state.selected)
  return defaultModelRef({ ...store.state, selected: undefined }).split("#", 1)[0]
}

/**
 * Apply one `/effort` choice so the open session's next request uses it, and
 * remember it on the server so the next start brings it back. The choice is
 * saved on the layer that decides the session's effort: a `#suffix` outranks
 * everything, so it is dropped first; an Agent-level effort outranks the
 * per-model preference, so then the Agent's effort (`SetAgentEffort`) is
 * changed, else the model's (`SetModelEffortPreference`). `default` clears
 * both remembered layers. Works while a turn runs: the engine resolves the
 * effort per request round.
 */
export async function selectEffort(context: CommandContext, effort: string): Promise<void> {
  const { store, client, actions } = context
  if (store.state.selected?.parent) {
    store.setStatus(revertReadOnlyStatus)
    return
  }
  const base = effortModelBase(context)
  if (!base) throw new Error("No model is available; configure a provider on the backend")
  const [providerId, ...modelParts] = base.split("/")
  const modelId = modelParts.join("/")
  if (!providerId || !modelId) throw new Error(`Invalid model reference ${base}`)
  const model = store.state.models.find((row) => row.id === base)
  if (!isKnownEffort(model, effort)) throw new Error(`Unknown thinking effort ${effort} for ${base}`)
  const value = effort === "default" ? "" : effort
  let session = store.state.selected
  if (session?.model?.variant) {
    session = await client.updateSessionModel(session.id, base)
    store.setSelected(session)
  }
  const agentDecides = session?.effortSource === "EFFORT_SOURCE_AGENT"
  if (session && agentDecides) await client.setAgentEffort(session.agent, value)
  if (!agentDecides || !value) await client.setModelEffortPreference(providerId, modelId, value)
  if (session) store.setSelected(await client.request<SessionInfo>("GET", `/v2/sessions/${encodeURIComponent(session.id)}`))
  store.setStatus(`Thinking effort → ${effort}`)
  await actions.refresh()
}

/** Open the server-backed effort picker for the active model. */
export function openEffortPicker(context: CommandContext): void {
  const { store, actions } = context
  if (store.state.selected?.parent) {
    store.setStatus(revertReadOnlyStatus)
    return
  }
  const base = effortModelBase(context)
  if (!base) throw new Error("No model is available; configure a provider on the backend")
  const session = store.state.selected
  const model = session ? currentModel(store.state) : store.state.models.find((row) => row.id === base)
  const effective = session?.effectiveEffort || model?.reasoningDefault || "default"
  actions.openPicker({
    title: "Thinking effort",
    rows: effortRows(model, session?.effectiveEffort || undefined, effective),
    onSelect: (row) => selectEffort(context, row.id),
  })
}

/** `/effort` (and `/think`): open the picker, or apply the named effort directly. */
const effortArgumentHint = "[default|none|minimal|low|medium|high|xhigh|max]"
const effortCompletion = ({ current, head }: ArgumentPosition): string[] =>
  matchValues(head, current, ["default", "none", "minimal", "low", "medium", "high", "xhigh", "max"])
const runEffort = (context: CommandContext, { args }: CommandInvocation): Promise<void> | void =>
  args[0] ? selectEffort(context, args[0]) : openEffortPicker(context)

export const modelCommands: CommandSpec[] = [
  {
    name: "/models",
    description: "List models",
    run: async ({ store, actions }) => { store.setView("models"); await actions.refresh() },
  },
  {
    name: "/model",
    description: "Pick a session model (models grouped by provider), or set one directly; with no session the choice is remembered for the next one",
    argumentHint: "[provider/model]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.models) : [],
    run: async ({ store, client, actions }, { args }) => {
      if (store.state.selected?.parent) {
        store.setStatus(revertReadOnlyStatus)
        return
      }
      const selected = store.state.selected
      if (args[0]) {
        const reference = args[0]
        const invalid = invalidEffortSuffix(reference, store.state.models)
        if (invalid !== undefined) {
          throw new Error(invalid ? `Unknown thinking effort ${invalid} for ${reference.split("#", 1)[0]}` : `Model reference ${args[0]} ends with an empty "#effort" suffix`)
        }
        if (!selected) {
          store.setPendingModel(reference)
          store.setStatus(`Model → ${reference} · applies when the session is created`)
          return
        }
        const updated = await client.updateSessionModel(selected.id, reference)
        store.setSelected(updated)
        await persistAgentModel({ store, client, actions }, updated, reference)
        // The server's reply, not the store: a racing session-list read may have replaced it.
        store.setStatus(`Model → ${modelReference(updated) || reference}`)
        await actions.refresh()
        return
      }
      openModelPicker({ store, client, actions })
    },
  },
  {
    name: "/effort",
    description: "Pick the thinking effort for the current model; choices are remembered per model",
    argumentHint: effortArgumentHint,
    complete: effortCompletion,
    run: runEffort,
  },
  {
    // Kept for muscle memory from backends that advertised a `think`
    // placeholder (removed from the catalog); if a backend row ever
    // re-appears, the local registry claims the name first
    // (commands/menu.ts drops clashing backend rows).
    name: "/think",
    description: "Set the thinking effort (alias of /effort)",
    argumentHint: effortArgumentHint,
    complete: effortCompletion,
    run: runEffort,
  },
  {
    name: "/agent",
    description: "Open the Agents view (primary, subagent, and system agents: select, model, effort), or switch to one directly; with no session the choice is remembered for the next one",
    argumentHint: "[name]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.agents) : [],
    run: async (context, { args }) => {
      if (args[0]) {
        await selectAgent(context, args[0])
        return
      }
      context.actions.openAgents()
    },
  },
]

export const keyCommand: CommandSpec = {
  name: "/key",
  description: "Open the Provider View: providers, keys, add a provider, fetch and test models, edit model metadata",
  run: ({ actions }) => { actions.openProviders() },
}
