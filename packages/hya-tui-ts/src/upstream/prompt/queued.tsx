import { createSignal } from "solid-js"
import { createSimpleContext } from "../context/helper"
import {
  createQueuedPromptStore,
  type EnqueueQueuedPrompt,
  type QueuedPrompt,
  type QueuedPromptStore,
} from "../../hya/queued-prompts"

export type { EnqueueQueuedPrompt, QueuedPrompt }

export const { use: useQueuedPrompts, provider: QueuedPromptProvider } = createSimpleContext({
  name: "QueuedPrompts",
  init: () => {
    const store: QueuedPromptStore = createQueuedPromptStore()
    const [version, setVersion] = createSignal(0)
    const touch = () => setVersion((value) => value + 1)

    return {
      list(sessionID: string) {
        version()
        return store.list(sessionID)
      },
      enqueue(input: EnqueueQueuedPrompt) {
        const item = store.enqueue(input)
        touch()
        return item
      },
      remove(id: string) {
        const item = store.remove(id)
        touch()
        return item
      },
      dequeue(sessionID: string) {
        const item = store.dequeue(sessionID)
        touch()
        return item
      },
      clear(sessionID: string) {
        store.clear(sessionID)
        touch()
      },
      pauseDrain() {
        store.pauseDrain()
        touch()
      },
      resumeDrain() {
        store.resumeDrain()
        touch()
      },
      isDrainPaused() {
        version()
        return store.isDrainPaused()
      },
    }
  },
})
