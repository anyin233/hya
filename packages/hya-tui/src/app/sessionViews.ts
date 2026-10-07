/** Reference-counted child transcript watches. The server projection remains authoritative. */
import { createRoot } from "solid-js"
import type { HyaClient, SessionInfo } from "../client"
import { createAppStore, type AppStore } from "../state/store"

type ViewClient = Pick<HyaClient, "request" | "listMessages" | "listEventsSince" | "streamSession">
export interface SessionView { store: AppStore; release(): void }
interface Watch { store: AppStore; refs: number; abort: AbortController; dispose(): void }
export class SessionViews {
  private watches = new Map<string, Watch>()
  constructor(private client: ViewClient) {}
  acquire(scope: string, id: string): SessionView {
    const key = JSON.stringify([scope, id])
    let watch = this.watches.get(key)
    if (!watch) {
      let dispose = () => {}
      const store = createRoot((cleanup) => { dispose = cleanup; return createAppStore() })
      store.setConnected(false)
      store.setStatus("Loading subagent transcript…")
      watch = { store, refs: 0, abort: new AbortController(), dispose }
      this.watches.set(key, watch)
      void this.run(watch, id)
    }
    watch.refs++
    const current = watch
    let released = false
    return { store: current.store, release: () => {
      if (released) return
      released = true
      if (--current.refs === 0) {
        current.abort.abort(); current.dispose()
        if (this.watches.get(key) === current) this.watches.delete(key)
      }
    } }
  }
  dispose(): void {
    for (const watch of this.watches.values()) { watch.abort.abort(); watch.dispose() }
    this.watches.clear()
  }
  private async run(watch: Watch, id: string): Promise<void> {
    const { store, abort } = watch
    let refresh: ReturnType<typeof setTimeout> | undefined
    let flush: ReturnType<typeof setTimeout> | undefined
    let reading = false, again = false
    const alive = () => !abort.signal.aborted
    const read = async () => {
      if (!alive()) return
      if (reading) { again = true; return }
      reading = true
      try {
        do {
          again = false
          const [session, messages] = await Promise.all([
            this.client.request<SessionInfo>("GET", `/v1/sessions/${encodeURIComponent(id)}`), this.client.listMessages(id),
          ])
          if (!alive()) return
          store.setSelected(session); store.setMembers(session.members ?? []); store.setMessages(id, messages)
          store.setStatus("")
        } while (again && alive())
      } finally { reading = false }
    }
    const scheduleFlush = () => {
      if (flush || !alive()) return
      flush = setTimeout(() => { flush = undefined; if (alive()) store.flushOverlay() }, 16)
    }
    const schedule = () => {
      if (refresh || !alive()) return
      refresh = setTimeout(() => { refresh = undefined; void read().catch((error: unknown) => {
        if (alive()) store.setStatus(`Transcript refresh failed: ${String(error)}`)
      }) }, 80)
    }
    abort.signal.addEventListener("abort", () => { if (refresh) clearTimeout(refresh); if (flush) clearTimeout(flush) }, { once: true })
    while (alive()) {
      try {
        if (!store.state.selected) {
          const session = await this.client.request<SessionInfo>("GET", `/v1/sessions/${encodeURIComponent(id)}`)
          if (!alive()) return
          store.openSession(session)
        }
        await this.client.streamSession(id, store.fold.lastSeq, (frame) => {
          if (!alive()) return
          if (frame.resync) { store.markLiveLost(); throw new Error("Transcript resync requested") }
          if (!frame.event) return
          const effect = store.applyEvent(frame.event)
          if (effect.changed) scheduleFlush()
          if (effect.durable || effect.finished) schedule()
        }, abort.signal, async () => {
          const events = await this.client.listEventsSince(id, store.fold.lastSeq)
          if (!alive()) return
          for (const event of events) store.applyEvent(event)
          store.flushOverlay()
          await read()
          if (alive()) store.setConnected(true)
        })
        if (alive()) throw new Error("Transcript stream disconnected")
      } catch (error) {
        if (!alive()) return
        store.setConnected(false); store.markLiveLost()
        store.setStatus(`Reconnecting: ${String(error)}`)
        await new Promise<void>((resolve) => {
          const done = () => { clearTimeout(timer); abort.signal.removeEventListener("abort", done); resolve() }
          const timer = setTimeout(done, 800)
          abort.signal.addEventListener("abort", done, { once: true })
        })
      }
    }
  }
}
