import type { TuiPluginApi, TuiPluginMeta, TuiPluginStatus, TuiSlotPlugin } from "@opencode-ai/plugin/tui"

import { createBuiltinPlugins } from "../upstream/feature-plugins/builtins"
import type { TuiPluginHost } from "../upstream/plugin/runtime"

export function createStaticPluginHost(): TuiPluginHost {
  let dispose: (() => Promise<void>) | undefined

  return {
    async start(input) {
      const slots = input.runtime.setupSlots(input.api)
      const cleanups: Array<() => void | Promise<void>> = []
      const statuses: TuiPluginStatus[] = []
      dispose = async () => {
        for (const cleanup of cleanups.reverse()) await cleanup()
        slots.dispose()
        input.dispose?.()
        input.runtime.clear()
      }

      for (const plugin of createBuiltinPlugins()) {
        if (plugin.enabled === false) continue
        const controller = new AbortController()
        const track = <T extends () => void | Promise<void>>(cleanup: T) => {
          cleanups.push(cleanup)
          return cleanup
        }
        const lifecycle: TuiPluginApi["lifecycle"] = {
          signal: controller.signal,
          onDispose(fn) {
            track(fn)
            return () => {
              const index = cleanups.indexOf(fn)
              if (index >= 0) cleanups.splice(index, 1)
            }
          },
        }
        const now = Date.now()
        const meta: TuiPluginMeta = {
          id: plugin.id,
          source: "internal",
          spec: plugin.id,
          target: plugin.id,
          first_time: now,
          last_time: now,
          time_changed: now,
          load_count: 1,
          fingerprint: plugin.id,
          state: "first",
        }
        const pluginSlots: TuiPluginApi["slots"] = {
          register<Slots extends Record<string, object>>(slot: TuiSlotPlugin<Slots>) {
            const id = `${plugin.id}:${cleanups.length}`
            track(slots.register({ ...slot, id }))
            return id
          },
        }
        const api: TuiPluginApi = {
          ...input.api,
          lifecycle,
          slots: pluginSlots,
          route: {
            register: (routes) => track(input.api.route.register(routes)),
            navigate: input.api.route.navigate,
            get current() {
              return input.api.route.current
            },
          },
          event: {
            on: (type, handler) => track(input.api.event.on(type, handler)),
          },
          keymap: new Proxy(input.api.keymap, {
            get(target, property, receiver) {
              const value = Reflect.get(target, property, receiver)
              if (property !== "registerLayer") return typeof value === "function" ? value.bind(target) : value
              return (...args: Parameters<typeof target.registerLayer>) => track(target.registerLayer(...args))
            },
          }),
        }
        await plugin.tui(api, undefined, meta)
        track(() => controller.abort())
        statuses.push({ id: plugin.id, source: "internal", spec: plugin.id, target: plugin.id, enabled: true, active: true })
      }

      input.runtime.update({ status: statuses })
    },
    async dispose() {
      await dispose?.()
      dispose = undefined
    },
  }
}
