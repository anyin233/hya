import type {
  AgentV2Info,
  CommandV2Info,
  Event,
  IntegrationInfo,
  LocationRef,
  ModelV2Info,
  ProviderV2Info,
  ReferenceInfo,
  SkillV2Info,
} from "@opencode-ai/sdk/v2"
import { createStore } from "solid-js/store"
import { createSimpleContext } from "./helper"
import { useSDK } from "./sdk"
import { useEvent } from "./event"
import { createSignal, onCleanup, onMount } from "solid-js"

type V2Event = Event extends infer Item
  ? Item extends { properties: infer Properties }
    ? Omit<Item, "properties"> & { data: Properties; location?: LocationRef }
    : never
  : never

type LocationData = {
  agent?: AgentV2Info[]
  command?: CommandV2Info[]
  integration?: IntegrationInfo[]
  model?: ModelV2Info[]
  provider?: ProviderV2Info[]
  reference?: ReferenceInfo[]
  skill?: SkillV2Info[]
}

type Data = {
  location: Record<string, LocationData>
}

function locationKey(location: LocationRef) {
  return JSON.stringify([location.directory, location.workspaceID])
}

function locationQuery(ref?: LocationRef) {
  return ref ? { directory: ref.directory, workspace: ref.workspaceID } : undefined
}

export const { use: useData, provider: DataProvider } = createSimpleContext({
  name: "Data",
  init: () => {
    const [store, setStore] = createStore<Data>({ location: {} })

    const sdk = useSDK()
    const events = useEvent()
    const [defaultLocation, setDefaultLocation] = createSignal<LocationRef>({
      directory: sdk.directory ?? process.cwd(),
    })


    function handleEvent(event: V2Event) {
      switch (event.type) {
        case "catalog.updated":
          void Promise.all([
            result.location.model.refresh(event.location),
            result.location.provider.refresh(event.location),
          ])
          break
        case "reference.updated":
          void result.location.reference.refresh()
          break
        case "integration.updated":
          void Promise.all([
            result.location.integration.refresh(event.location),
            result.location.model.refresh(event.location),
            result.location.provider.refresh(event.location),
          ])
          break
      }
    }

    onMount(() => {
      const unsub = events.subscribe((event, metadata) => {
        handleEvent({
          ...event,
          data: event.properties,
          location: { directory: metadata.directory, workspaceID: metadata.workspace },
        } as V2Event)
      })
      onCleanup(unsub)
    })

    const result = {
      location: {
        default() {
          return defaultLocation()
        },
        async refresh(ref?: LocationRef) {
          const response = await sdk.client.v2.location.get({ location: locationQuery(ref) }, { throwOnError: true })
          const location = response.data
          const key = locationKey(location)
          if (!store.location[key]) setStore("location", key, {})
          if (!ref) setDefaultLocation({ directory: location.directory, workspaceID: location.workspaceID })
        },
        agent: {
          list(location?: LocationRef) {
            return store.location[locationKey(location ?? defaultLocation())]?.agent
          },
          async refresh(ref?: LocationRef) {
            const result = await sdk.client.v2.agent.list({ location: locationQuery(ref) }, { throwOnError: true })
            const key = locationKey(result.data.location)
            setStore("location", key, "agent", result.data.data)
          },
        },
        command: {
          list(location?: LocationRef) {
            return store.location[locationKey(location ?? defaultLocation())]?.command
          },
          async refresh(ref?: LocationRef) {
            const result = await sdk.client.v2.command.list({ location: locationQuery(ref) }, { throwOnError: true })
            const key = locationKey(result.data.location)
            setStore("location", key, "command", result.data.data)
          },
        },
        integration: {
          list(location?: LocationRef) {
            return store.location[locationKey(location ?? defaultLocation())]?.integration
          },
          async refresh(ref?: LocationRef) {
            const result = await sdk.client.v2.integration.list(
              { location: locationQuery(ref) },
              { throwOnError: true },
            )
            const key = locationKey(result.data.location)
            setStore("location", key, "integration", result.data.data)
          },
        },
        model: {
          list(location?: LocationRef) {
            return store.location[locationKey(location ?? defaultLocation())]?.model
          },
          async refresh(ref?: LocationRef) {
            const result = await sdk.client.v2.model.list({ location: locationQuery(ref) }, { throwOnError: true })
            const key = locationKey(result.data.location)
            setStore("location", key, "model", result.data.data)
          },
        },
        provider: {
          list(location?: LocationRef) {
            return store.location[locationKey(location ?? defaultLocation())]?.provider
          },
          async refresh(ref?: LocationRef) {
            const result = await sdk.client.v2.provider.list({ location: locationQuery(ref) }, { throwOnError: true })
            const key = locationKey(result.data.location)
            setStore("location", key, "provider", result.data.data)
          },
        },
        reference: {
          list(location?: LocationRef) {
            return store.location[locationKey(location ?? defaultLocation())]?.reference
          },
          async refresh(ref?: LocationRef) {
            const result = await sdk.client.v2.reference.list({ location: locationQuery(ref) }, { throwOnError: true })
            const key = locationKey(result.data.location)
            setStore("location", key, "reference", result.data.data)
          },
        },
        skill: {
          list(location?: LocationRef) {
            return store.location[locationKey(location ?? defaultLocation())]?.skill
          },
          async refresh(ref?: LocationRef) {
            const result = await sdk.client.v2.skill.list({ location: locationQuery(ref) }, { throwOnError: true })
            const key = locationKey(result.data.location)
            setStore("location", key, "skill", result.data.data)
          },
        },
      },
    }

    onMount(() => {
      void Promise.allSettled([
        result.location.refresh(),
        result.location.agent.refresh(),
        result.location.integration.refresh(),
        result.location.model.refresh(),
        result.location.provider.refresh(),
        result.location.reference.refresh(),
        result.location.command.refresh(),
        result.location.skill.refresh(),
      ]).then((settled) => {
        for (const failure of settled.filter((item) => item.status === "rejected"))
          console.error("Failed to refresh default location data", failure.reason)
      })
    })

    return result
  },
})
