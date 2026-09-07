import { createMemo, createSignal } from "solid-js"
import { TextAttributes } from "@opentui/core"
import { useLocal } from "../context/local"
import { map, pipe, flatMap, entries, filter, sortBy } from "remeda"
import { DialogSelect } from "../ui/dialog-select"
import { useDialog } from "../ui/dialog"
import { DialogVariant } from "./dialog-variant"
import * as fuzzysort from "fuzzysort"
import { useConnected } from "./use-connected"
import { useSync } from "../context/sync"
import { useToast } from "../ui/toast"
import { agentModelPickerTitle } from "../../hya/agent-models"
import { catalogProviderStatus, decodeCatalogProviders } from "../../hya/model-catalog"
import { useTheme } from "../context/theme"

/**
 * Select a model for the active root Agent or an explicit Agent target.
 * @param props Optional provider filter and stable target Agent id.
 * @returns The shared model-selection dialog.
 */
export function DialogModel(props: { providerID?: string; agentID?: string }) {
  const local = useLocal()
  const sync = useSync()
  const dialog = useDialog()
  const [query, setQuery] = createSignal("")
  const toast = useToast()
  const { theme } = useTheme()
  const target = createMemo(() => {
    const agentID = props.agentID ?? local.agent.current()?.name
    return agentID ? sync.data.agentModels.find((item) => item.agentID === agentID) : undefined
  })
  const [saving, setSaving] = createSignal(false)

  const connected = useConnected()
  const showExtra = createMemo(() => connected() && !props.providerID)
  const catalogProviders = createMemo(() => decodeCatalogProviders(sync.data.provider))
  const catalogMembership = createMemo(
    () =>
      new Map(
        catalogProviders().map((provider) => [
          provider.id,
          new Set(provider.models.map((model) => model.modelID)),
        ]),
      ),
  )
  const providerStatus = (providerID: string) => {
    const provider = catalogProviders().find((item) => item.id === providerID)
    return provider ? catalogProviderStatus(provider) : "Unavailable"
  }

  const options = createMemo(() => {
    const needle = query().trim()
    const showSections = showExtra() && needle.length === 0
    const favorites = connected() ? local.model.favorite() : []
    const recents = local.model.recent()

    function toOptions(items: typeof favorites, category: string) {
      if (!showSections) return []
      return items.flatMap((item) => {
        const provider = sync.data.provider.find((provider) => provider.id === item.providerID)
        if (!provider) return []
        const model = provider.models[item.modelID]
        if (!model) return []
        return [
          {
            key: item,
            value: { providerID: provider.id, modelID: model.id },
            title: model.name ?? item.modelID,
            description: provider.name,
            category,
            onSelect: () => {
              onSelect(provider.id, model.id)
            },
          },
        ]
      })
    }

    const favoriteOptions = toOptions(favorites, "Favorites")
    const recentOptions = toOptions(
      recents.filter(
        (item) => !favorites.some((fav) => fav.providerID === item.providerID && fav.modelID === item.modelID),
      ),
      "Recent",
    )

    const providerOptions = pipe(
      sync.data.provider,
      sortBy((provider) => provider.name),
      flatMap((provider) =>
        pipe(
          provider.models,
          entries(),
          filter(([model]) => catalogMembership().get(provider.id)?.has(model) === true),
          filter(([_, info]) => info.status !== "deprecated"),
          filter(([_, info]) => (props.providerID ? info.providerID === props.providerID : true)),
          map(([model, info]) => ({
            value: { providerID: provider.id, modelID: model },
            title: info.name ?? model,
            releaseDate: info.release_date,
            description: favorites.some((item) => item.providerID === provider.id && item.modelID === model)
              ? `(Favorite) · ${providerStatus(provider.id)}`
              : `${provider.name} · ${providerStatus(provider.id)}`,
            category: connected() ? provider.name : undefined,
            onSelect() {
              onSelect(provider.id, model)
            },
          })),
          filter((option) => {
            if (!showSections) return true
            if (
              favorites.some(
                (item) => item.providerID === option.value.providerID && item.modelID === option.value.modelID,
              )
            )
              return false
            if (
              recents.some(
                (item) => item.providerID === option.value.providerID && item.modelID === option.value.modelID,
              )
            )
              return false
            return true
          }),
          (options) => sortModelOptions(options, props.providerID !== undefined),
        ),
      ),
    )

    if (needle) {
      return fuzzysort.go(needle, providerOptions, { keys: ["title", "category"] }).map((x) => x.obj)
    }

    return [...favoriteOptions, ...recentOptions, ...providerOptions]
  })

  const provider = createMemo(() =>
    props.providerID ? sync.data.provider.find((item) => item.id === props.providerID) : null,
  )

  const current = createMemo(() => {
    const agentID = props.agentID ?? local.agent.current()?.name
    const scoped = agentID ? local.model.currentForAgent(agentID) : undefined
    if (scoped) return scoped
    const effective = target()?.effective
    if (effective) return { providerID: effective.providerID, modelID: effective.modelID }
    return local.model.current()
  })

  const title = createMemo(() => {
    if (props.agentID) return agentModelPickerTitle(props.agentID)
    const value = provider()
    if (!value) return "Select model"
    return value.name
  })

  /**
   * Apply one model selection to the explicit target or active root Agent.
   * @param providerID Exact provider id from the shared catalog.
   * @param modelID Exact provider-local base-model id.
   * @returns Nothing; asynchronous persistence keeps the dialog locked in place.
   */
  function onSelect(providerID: string, modelID: string): void {
    const model = { providerID, modelID }
    if (saving()) return
    setSaving(true)
    const pending = props.agentID
      ? local.model.selectForAgent(props.agentID, model)
      : local.model.select(model, { recent: true })
    void pending.then(
      (selected) => {
        if (!selected) {
          setSaving(false)
          return
        }
        if (local.agent.current()?.name === (props.agentID ?? local.agent.current()?.name)) {
          local.model.variant.set(undefined)
        }
        setSaving(false)
        if (props.agentID) {
          dialog.clear()
          return
        }
        const committed = local.model.current()
        if (committed?.providerID !== model.providerID || committed.modelID !== model.modelID) {
          local.model.variant.set(undefined)
        }
        const list = local.model.variant.list()
        const cur = local.model.variant.selected()
        if (cur === "default" || (cur && list.includes(cur))) {
          dialog.clear()
          return
        }
        if (list.length > 0) {
          dialog.replace(() => <DialogVariant />)
          return
        }
        dialog.clear()
      },
      (error) => {
        setSaving(false)
        toast.show({
          variant: "error",
          message: error instanceof Error ? error.message : String(error),
          duration: 5000,
        })
      },
    )
  }

  const canSaveConfiguration = createMemo(
    () => sync.data.capabilities.agentModelConfiguration && target() !== undefined,
  )

  const provenance = createMemo(() => {
    const row = target()
    if (!row) return ""
    const agentID = props.agentID ?? local.agent.current()?.name
    const scoped = agentID ? local.model.currentForAgent(agentID) : undefined
    const effective = scoped ?? row.effective
    const effectiveID = `${effective.providerID}/${effective.modelID}`
    const backendID = `${row.effective.providerID}/${row.effective.modelID}`
    const source =
      row.sessionOverride || (scoped && effectiveID !== backendID)
        ? `${local.model.scope() === "draft" ? "Draft" : "Session"} override: ${effectiveID}`
        : `${row.effective.source}: ${effectiveID}`
    if (row.configurationPath) return `${source} · configured default: ${row.configurationPath}`
    return source
  })

  return (
    <DialogSelect<ReturnType<typeof options>[number]["value"]>
      options={options()}
      actions={[
        {
          command: "model.dialog.favorite",
          title: "Favorite",
          hidden: !connected(),
          onTrigger: (option) => {
            local.model.toggleFavorite(option.value as { providerID: string; modelID: string })
          },
        },
        {
          command: "model.dialog.save_config",
          title: "Save configured default",
          hidden: !canSaveConfiguration(),
          onTrigger: (option) => {
            const agentID = props.agentID ?? local.agent.current()?.name
            if (!agentID) return
            setSaving(true)
            void local.model
              .saveConfiguredDefault(agentID, option.value as { providerID: string; modelID: string })
              .finally(() => setSaving(false))
          },
        },
      ]}
      onFilter={setQuery}
      flat={true}
      skipFilter={true}
      locked={saving()}
      title={title()}
      titleView={
        <box flexGrow={1} flexShrink={1} gap={1}>
          <text attributes={TextAttributes.BOLD}>{title()}</text>
          <text fg={theme.textMuted}>{provenance()}</text>
        </box>
      }
      current={current()}
    />
  )
}

export function sortModelOptions<T extends { footer?: string; releaseDate: string | number; title: string }>(
  options: T[],
  newestFirst: boolean,
) {
  if (newestFirst) return sortBy(options, [(option) => option.releaseDate, "desc"], (option) => option.title)
  return sortBy(
    options,
    (option) => option.footer !== "Free",
    (option) => option.title,
  )
}
