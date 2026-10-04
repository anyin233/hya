/**
 * The VM thread of the shared extension host (src/main.ts): one VM per
 * extension. Requests for one extension run one at a time; a VM spinning
 * until its deadline holds this thread, never the host's channel. `api.fs`
 * calls go to the main thread, which asks the TUI.
 */
import { ExtensionVm, quickJsModule } from "./host"
import type { FromWorker, ToWorker } from "./hostMessages"

declare const self: Worker

const vms = new Map<string, ExtensionVm>()
/** The tail of each extension's request chain. */
const queues = new Map<string, Promise<void>>()
const hostCalls = new Map<number, PromiseWithResolvers<unknown>>()
let nextCall = 1

const post = (message: FromWorker): void => self.postMessage(message)

// Compile now, before the first load arrives; a failure surfaces at that load.
quickJsModule().catch(() => undefined)

function enqueue(ext: string, work: () => Promise<void>): void {
  const next = (queues.get(ext) ?? Promise.resolve()).then(work, work)
  queues.set(ext, next)
  void next.finally(() => { if (queues.get(ext) === next) queues.delete(ext) })
}

self.onmessage = (event: MessageEvent<ToWorker>) => {
  const message = event.data
  switch (message.type) {
    case "load":
      enqueue(message.ext, async () => {
        try {
          vms.get(message.ext)?.dispose()
          vms.set(message.ext, await ExtensionVm.create(message.script, {
            log: (line) => post({ type: "log", ext: message.ext, line }),
            hostCall: (method, params) => {
              const call = nextCall++
              const waiter = Promise.withResolvers<unknown>()
              hostCalls.set(call, waiter)
              post({ type: "host", call, ext: message.ext, method, params })
              return waiter.promise
            },
          }))
          post({ type: "done", seq: message.seq })
        } catch (error) {
          post({ type: "done", seq: message.seq, error: error instanceof Error ? error.message : String(error) })
        }
      })
      return
    case "unload":
      enqueue(message.ext, async () => {
        vms.get(message.ext)?.dispose()
        vms.delete(message.ext)
        post({ type: "done", seq: message.seq })
      })
      return
    case "call":
      enqueue(message.ext, async () => {
        const vm = vms.get(message.ext)
        if (!vm) {
          post({ type: "done", seq: message.seq, error: `extension not loaded: ${message.ext}` })
          return
        }
        post({ type: "done", seq: message.seq, response: await vm.handle(message.request, message.id) })
      })
      return
    case "hostResult": {
      const waiter = hostCalls.get(message.call)
      hostCalls.delete(message.call)
      if (message.error !== undefined) waiter?.reject(new Error(message.error))
      else waiter?.resolve(message.value)
      return
    }
  }
}
